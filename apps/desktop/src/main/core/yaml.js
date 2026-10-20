// 轻量 YAML 子集解析器：覆盖 SKILL.md frontmatter / 插件清单 / 配置文件的常见写法。
// 支持：块映射、块序列、行内 [..] 与 {..}、引号字符串、多行 | 与 >、注释、布尔与数字。

function stripComment(line) {
  let out = '';
  let quote = null;
  for (let i = 0; i < line.length; i++) {
    const ch = line[i];
    if (quote) {
      out += ch;
      if (ch === quote) quote = null;
      continue;
    }
    if (ch === '"' || ch === "'") { quote = ch; out += ch; continue; }
    if (ch === '#' && (i === 0 || /\s/.test(line[i - 1]))) break;
    out += ch;
  }
  return out.replace(/\s+$/, '');
}

function splitKey(text) {
  let quote = null;
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (quote) { if (ch === quote) quote = null; continue; }
    if (ch === '"' || ch === "'") { quote = ch; continue; }
    if (ch === ':' && (i === text.length - 1 || /\s/.test(text[i + 1]))) {
      return [text.slice(0, i).trim(), text.slice(i + 1).trim()];
    }
  }
  return null;
}

export function parseScalar(raw) {
  const s = String(raw || '').trim();
  if (s === '') return '';
  if (s === '~' || s === 'null' || s === 'Null' || s === 'NULL') return null;
  if (s === 'true' || s === 'True' || s === 'TRUE') return true;
  if (s === 'false' || s === 'False' || s === 'FALSE') return false;
  if (/^-?\d+$/.test(s)) return Number(s);
  if (/^-?\d*\.\d+$/.test(s)) return Number(s);
  if ((s.startsWith('"') && s.endsWith('"')) || (s.startsWith("'") && s.endsWith("'"))) {
    const inner = s.slice(1, -1);
    return s.startsWith('"') ? inner.replace(/\\n/g, '\n').replace(/\\"/g, '"').replace(/\\\\/g, '\\') : inner.replace(/''/g, "'");
  }
  if (s.startsWith('[') && s.endsWith(']')) {
    const inner = s.slice(1, -1).trim();
    if (!inner) return [];
    return splitInline(inner).map(parseScalar);
  }
  if (s.startsWith('{') && s.endsWith('}')) {
    const inner = s.slice(1, -1).trim();
    const obj = {};
    if (!inner) return obj;
    for (const part of splitInline(inner)) {
      const kv = splitKey(part);
      if (kv) obj[parseScalar(kv[0])] = parseScalar(kv[1]);
    }
    return obj;
  }
  return s;
}

function splitInline(text) {
  const out = [];
  let cur = '';
  let quote = null;
  let depth = 0;
  for (const ch of text) {
    if (quote) { cur += ch; if (ch === quote) quote = null; continue; }
    if (ch === '"' || ch === "'") { quote = ch; cur += ch; continue; }
    if (ch === '[' || ch === '{') depth++;
    if (ch === ']' || ch === '}') depth--;
    if (ch === ',' && depth === 0) { out.push(cur.trim()); cur = ''; continue; }
    cur += ch;
  }
  if (cur.trim()) out.push(cur.trim());
  return out;
}

export function parseYaml(text) {
  const src = String(text || '').replace(/\r\n/g, '\n').replace(/\t/g, '  ');
  const lines = src.split('\n');
  const root = {};
  const stack = [{ indent: -1, value: root, type: 'map' }];

  let i = 0;
  while (i < lines.length) {
    const rawLine = lines[i];
    const line = stripComment(rawLine);
    i++;
    if (!line.trim()) continue;
    if (line.trim() === '---' || line.trim() === '...') continue;
    const indent = line.length - line.trimStart().length;
    const content = line.trim();

    while (stack.length > 1 && indent <= stack[stack.length - 1].indent) stack.pop();
    const parent = stack[stack.length - 1];
    if (indent <= parent.indent && stack.length === 1) { /* root level */ }

    if (content.startsWith('- ') || content === '-') {
      const itemText = content === '-' ? '' : content.slice(2).trim();
      let arr = parent.value;
      if (!Array.isArray(arr)) {
        // 该键此前声明为空，替换为数组
        if (parent.assign) parent.assign([]);
        arr = [];
        parent.value = arr;
        parent.type = 'seq';
      }
      if (!itemText) {
        const child = {};
        arr.push(child);
        stack.push({ indent, value: child, type: 'map', assign: (v) => { arr[arr.length - 1] = v; } });
      } else if (!splitKey(itemText) && !itemText.startsWith('- ')) {
        // 标量序列项（如 - a、- \"text\"），而不是嵌套映射
        arr.push(parseScalar(itemText));
      } else {
        const kv = splitKey(itemText);
        if (kv) {
          const obj = {};
          arr.push(obj);
          obj[kv[0]] = parseScalar(kv[1]);
          stack.push({ indent: indent + 2, value: obj, type: 'map', assign: (v) => { arr[arr.length - 1] = v; } });
        } else {
          arr.push(parseScalar(itemText));
        }
      }
      continue;
    }

    const kv = splitKey(content);
    if (!kv) continue;
    const key = parseScalar(kv[0]);
    const valueText = kv[1];

    const BLOCK_RE = /^[|>][+-]?$/;
    if (valueText === '' || BLOCK_RE.test(valueText)) {
      if (BLOCK_RE.test(valueText)) {
        const blockLines = [];
        while (i < lines.length) {
          const next = lines[i];
          if (next.trim() === '') { blockLines.push(''); i++; continue; }
          const ni = next.length - next.trimStart().length;
          if (ni <= indent) break;
          blockLines.push(next.slice(Math.min(ni, indent + 2)));
          i++;
        }
        let joined = blockLines.join('\n');
        if (valueText.startsWith('>')) joined = joined.replace(/([^\n])\n(?=[^\n])/g, '$1 ');
        if (valueText.endsWith('-')) joined = joined.replace(/\n+$/, '');
        parent.value[key] = joined;
        continue;
      }
      // 空值：可能是嵌套映射或序列；先按映射占位，若下一行是序列再替换
      const container = {};
      parent.value[key] = container;
      const assign = (v) => { parent.value[key] = v; };
      stack.push({ indent, value: container, type: 'map', assign, key });
      // 预读下一行：若是更缩进的普通文本（非 key: value、非列表项），
      // 说明这是 YAML 的「折叠标量」（如 description: 换行后接正文），应按字符串收集。
      let peekIdx = i;
      while (peekIdx < lines.length && !stripComment(lines[peekIdx]).trim()) peekIdx++;
      if (peekIdx < lines.length) {
        const peekLine = stripComment(lines[peekIdx]);
        const peekIndent = peekLine.length - peekLine.trimStart().length;
        const peek = peekLine.trim();
        const isList = peek.startsWith('- ') || peek === '-';
        const isKeyValue = Boolean(splitKey(peek));
        if (!isList && !isKeyValue && peekIndent > indent) {
          const collected = [];
          while (peekIdx < lines.length) {
            const raw = lines[peekIdx];
            const stripped = stripComment(raw);
            if (!stripped.trim()) { collected.push(''); peekIdx++; continue; }
            const ind = stripped.length - stripped.trimStart().length;
            if (ind <= indent) break;
            collected.push(stripped.trim());
            peekIdx++;
          }
          parent.value[key] = collected.join(' ').replace(/\s+/g, ' ').trim();
          i = peekIdx;
          stack.pop();
          continue;
        }
      }
      // 预读下一行判断序列
      if (i < lines.length) {
        const peek = stripComment(lines[i]).trim();
        if (peek.startsWith('- ') || peek === '-') {
          const arr = [];
          parent.value[key] = arr;
          stack[stack.length - 1].value = arr;
          stack[stack.length - 1].type = 'seq';
          stack[stack.length - 1].assign = assign;
        }
      }
      continue;
    }

    parent.value[key] = parseScalar(valueText);
  }
  return root;
}

export function stringifyYaml(value, indent) {
  const ind = indent || 0;
  const pad = ' '.repeat(ind);
  if (value === null || value === undefined) return 'null';
  if (typeof value === 'string') {
    if (!value) return "''";
    if (/[:#\n'"\[\]{}]|^\s|\s$/.test(value) || /^(true|false|null|\d+)$/i.test(value)) return JSON.stringify(value);
    return value;
  }
  if (typeof value === 'number' || typeof value === 'boolean') return String(value);
  if (Array.isArray(value)) {
    if (!value.length) return '[]';
    return '\n' + value.map((v) => pad + '- ' + String(stringifyYaml(v, ind + 2)).trimStart()).join('\n');
  }
  if (typeof value === 'object') {
    const keys = Object.keys(value);
    if (!keys.length) return '{}';
    return '\n' + keys.map((k) => {
      const v = value[k];
      const rendered = stringifyYaml(v, ind + 2);
      const inline = rendered.startsWith('\n') ? rendered : ' ' + rendered;
      return pad + k + ':' + inline;
    }).join('\n');
  }
  return String(value);
}

export function extractFrontmatter(text) {
  const src = String(text || '').replace(/^\uFEFF/, '');
  const m = src.match(/^---\r?\n([\s\S]*?)\r?\n---\r?\n?/);
  if (!m) return { data: {}, body: src, raw: '' };
  let data = {};
  try { data = parseYaml(m[1]) || {}; } catch { data = {}; }
  return { data, body: src.slice(m[0].length), raw: m[1] };
}
