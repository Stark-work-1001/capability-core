#!/usr/bin/env python
"""
capability-core embedder service
- Standalone HTTP embedding service for the hub's fast retrieval.
- Runs on its own venv (C:\external-assets\capability-core\embedder\venv).
- NEVER reuses memora's venv. Model weights are read-only shared.
- Listens on 0.0.0.0:18191  (memora 18181/18182 are never touched).
"""
import os
import sys
import time
import json
import logging

os.environ.setdefault("TOKENIZERS_PARALLELISM", "false")

MODEL_PATH = os.environ.get(
    "EMBEDDER_MODEL",
    "/mnt/d/.memory/model/Qwen3-Embedding-0.6B",
)
PORT = int(os.environ.get("EMBEDDER_PORT", "18191"))
HOST = os.environ.get("EMBEDDER_HOST", "0.0.0.0")

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s %(levelname)s [%(name)s] %(message)s",
)
log = logging.getLogger("embedder")

STATE = {
    "model_name": None,
    "dim": None,
    "device": None,
    "gpu": False,
    "load_seconds": None,
    "ready": False,
    "loaded_at": None,
    "cuda_device_name": None,
    "torch_version": None,
    "transformers_version": None,
}

_MODEL = None
_TOK = None


def log_banner():
    import torch
    import transformers

    STATE["torch_version"] = torch.__version__
    STATE["transformers_version"] = transformers.__version__
    STATE["gpu"] = bool(torch.cuda.is_available())
    if STATE["gpu"]:
        STATE["cuda_device_name"] = torch.cuda.get_device_name(0)
        STATE["device"] = "cuda"
    else:
        STATE["device"] = "cpu"

    global _MODEL, _TOK
    from transformers import AutoModel, AutoTokenizer

    t0 = time.time()
    log.info("loading tokenizer from %s", MODEL_PATH)
    _TOK = AutoTokenizer.from_pretrained(MODEL_PATH)
    log.info("loading model from %s", MODEL_PATH)
    _MODEL = AutoModel.from_pretrained(MODEL_PATH)
    if STATE["gpu"]:
        _MODEL = _MODEL.to("cuda")
    _MODEL.eval()
    dt = time.time() - t0

    STATE["dim"] = int(_MODEL.config.hidden_size)
    STATE["model_name"] = os.path.basename(MODEL_PATH.rstrip("/"))
    STATE["load_seconds"] = round(dt, 3)
    STATE["ready"] = True
    STATE["loaded_at"] = time.strftime("%Y-%m-%d %H:%M:%S")

    # Real, measured startup report -- no fake status.
    log.info("=" * 62)
    log.info("MODEL       : %s", STATE["model_name"])
    log.info("PATH        : %s", MODEL_PATH)
    log.info("DEVICE      : %s", STATE["device"])
    log.info("GPU         : %s", STATE["gpu"])
    if STATE["gpu"]:
        log.info("CUDA DEVICE : %s", STATE["cuda_device_name"])
    log.info("DIM         : %s", STATE["dim"])
    log.info("TORCH       : %s", STATE["torch_version"])
    log.info("TRANSFORMERS: %s", STATE["transformers_version"])
    log.info("LOAD SECONDS: %.3f", dt)
    if STATE["gpu"]:
        try:
            import torch as _t

            alloc = _t.cuda.memory_allocated(0) / 1024 / 1024
            log.info("VRAM ALLOC  : %.1f MiB", alloc)
        except Exception:
            pass
    log.info("=" * 62)
    log.info("listening on %s:%d", HOST, PORT)


def _last_token_pool(hidden, mask):
    left = _TOK.padding_side
    try:
        _TOK.padding_side = "left"
        h = hidden
        m = mask
        last = h[:, -1]
    finally:
        _TOK.padding_side = left
    return last


def embed_texts(texts):
    import torch
    import torch.nn.functional as F

    if not texts:
        return []

    kw = {}
    if _TOK.model_max_length and _TOK.model_max_length > 100000:
        kw["truncation"] = True
        kw["max_length"] = 8192
    else:
        kw["truncation"] = True
        kw["max_length"] = int(_TOK.model_max_length or 8192)

    batch = _TOK(texts, return_tensors="pt", padding=True, **kw)
    batch = {k: v.to(_MODEL.device) for k, v in batch.items()}

    with torch.no_grad():
        out = _MODEL(**batch)
        hidden = out.last_hidden_state
        mask = batch["attention_mask"]
        # Qwen3-Embedding uses last-token pooling
        if _TOK.padding_side == "left":
            emb = hidden[:, -1]
        else:
            lens = mask.sum(dim=1) - 1
            emb = hidden[torch.arange(hidden.size(0), device=hidden.device), lens]
        emb = F.normalize(emb, p=2, dim=1)

    emb = emb.float().cpu().numpy()
    return emb.tolist()


def create_app():
    from fastapi import FastAPI
    from pydantic import BaseModel, Field

    app = FastAPI(title="capability-core embedder", version="1.0.0")

    class EmbedRequest(BaseModel):
        texts: list = Field(..., description="list of strings to embed")

    @app.on_event("startup")
    def _startup():
        if not STATE["ready"]:
            log_banner()

    @app.get("/health")
    def health():
        return {
            "status": "ok" if STATE["ready"] else "loading",
            "model": STATE["model_name"],
            "model_path": MODEL_PATH,
            "dim": STATE["dim"],
            "device": STATE["device"],
            "gpu": STATE["gpu"],
            "cuda_device": STATE["cuda_device_name"],
            "torch_version": STATE["torch_version"],
            "transformers_version": STATE["transformers_version"],
            "load_seconds": STATE["load_seconds"],
            "loaded_at": STATE["loaded_at"],
            "port": PORT,
            "host": HOST,
        }

    @app.post("/embed")
    def embed(req: EmbedRequest):
        if not STATE["ready"]:
            return {"error": "model not ready", "vectors": [], "dim": STATE["dim"]}
        texts = [("" if t is None else str(t)) for t in req.texts]
        t0 = time.time()
        vecs = embed_texts(texts)
        dt = time.time() - t0
        log.info("embedded %d texts in %.3fs (%.2f texts/s)",
                 len(texts), dt, len(texts) / dt if dt > 0 else 0.0)
        return {"vectors": vecs, "dim": STATE["dim"], "count": len(vecs),
                "seconds": round(dt, 4)}

    return app


if __name__ == "__main__":
    import uvicorn
    app = create_app()
    uvicorn.run(app, host=HOST, port=PORT, log_level="info")
