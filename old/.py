import importlib
for m in ["sentence_transformers","onnxruntime","torch","transformers","numpy"]:
    try:
        mod = importlib.import_module(m)
        print(f"  {m}: {getattr(mod,'__version__','ok')}")
    except Exception as e:
        print(f"  {m}: MISSING")