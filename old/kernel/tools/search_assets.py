"""Dependency-free BM25 with CJK bigrams; JSON stdin/stdout only."""
import collections
import json
import math
import re
import sys

def terms(text):
    words = re.findall(r"[a-z0-9_]+|[\u3400-\u9fff]+", text.lower())
    out = []
    for word in words:
        if re.match(r"[\u3400-\u9fff]", word) and len(word) > 1:
            out.extend(word[i:i + 2] for i in range(len(word) - 1))
        else:
            out.append(word)
    return out

def search(data):
    docs = data["documents"]
    bags = [collections.Counter(terms(doc["text"])) for doc in docs]
    lengths = [sum(b.values()) for b in bags]
    avg = sum(lengths) / max(1, len(docs)) or 1
    df = collections.Counter(t for b in bags for t in b)
    query = set(terms(data["query"]))
    hits = []
    for doc, bag, size in zip(docs, bags, lengths):
        score = 0.0
        for term in query:
            tf = bag[term]
            if tf:
                idf = math.log(1 + (len(docs) - df[term] + .5) / (df[term] + .5))
                score += idf * tf * 2.2 / (tf + 1.2 * (.25 + .75 * size / avg))
        if score > 0:
            hits.append([doc["id"], score])
    return sorted(hits, key=lambda x: (-x[1], x[0]))[:80]

if __name__ == "__main__":
    sys.stdout.write(json.dumps(search(json.loads(sys.stdin.read())), ensure_ascii=True))
