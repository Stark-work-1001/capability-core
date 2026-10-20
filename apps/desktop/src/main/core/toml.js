// 极简 TOML 解析器：覆盖 config.toml 的常见结构（表、数组表、键值、行内数组）。
import { parseScalar } from './yaml.js';

function splitTop(text, sep) {
  const out = []; let cur = ''; let quote = null; let depth = 0;
  for (const ch of text) {
    if (quote) { cur += ch; if (ch === quote) quote = null; continue; }
    if (ch === '"' || ch === "'") { quote = ch; cur += ch; continue; }
    if (ch === '[' || ch === '{') depth++;
    if (ch === ']' || ch === '}') depth--;
    if (ch === sep && depth === 0) { out.push(cur); cur = ''; continue; }
    cur += ch;
  }
  if (cur.trim()) out.push(cur);
  return out;
}

export function parseToml(text) {
  const root = {};
  let cur = root;
  const lines = String(text || '').replace(/\r\n/g, '\n').split('\n');
  for (let raw of lines) {
    const line = raw.trim();
    if (!line || line.startsWith('#')) continue;
    const tableArray = line.match(/^\[\[([^\]]+)\]\]$/);
    if (tableArray) {
      const keys = tableArray[1].split('.').map((k) => k.trim());
      let node = root;
      for (let i = 0; i < keys.length - 1; i++) {
        node[keys[i]] = node[keys[i]] || {};
        node = node[keys[i]];
      }
      const leaf = keys[keys.length - 1];
      node[leaf] = node[leaf] || [];
      const item = {};
      node[leaf].push(item);
      cur = item;
      continue;
    }
    const table = line.match(/^\[([^\]]+)\]$/);
    if (table) {
      const keys = table[1].split('.').map((k) => k.trim().replace(/^["']|["']$/g, ''));
      let node = root;
      for (const k of keys) { node[k] = node[k] || {}; node = node[k]; }
      cur = node;
      continue;
    }
    const eq = splitTop(line, '=');
    if (eq.length < 2) continue;
    const key = eq[0].trim().replace(/^["']|["']$/g, '');
    const valueText = eq.slice(1).join('=').trim();
    let value;
    if (valueText.startsWith('[') && valueText.endsWith(']')) {
      value = splitTop(valueText.slice(1, -1), ',').map((p) => parseScalar(p.trim()));
    } else if (valueText.startsWith('{') && valueText.endsWith('}')) {
      const obj = {};
      for (const part of splitTop(valueText.slice(1, -1), ',')) {
        const [k, v] = splitTop(part, '=').length >= 2 ? [splitTop(part, '=')[0], splitTop(part, '=').slice(1).join('=')] : [part, ''];
        obj[k.trim()] = parseScalar(v.trim());
      }
      value = obj;
    } else {
      value = parseScalar(valueText);
    }
    cur[key] = value;
  }
  return root;
}
