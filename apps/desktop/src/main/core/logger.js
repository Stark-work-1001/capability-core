import fs from 'node:fs';
import path from 'node:path';

const LEVELS = { debug: 10, info: 20, warn: 30, error: 40 };

export class Logger {
  constructor(dir, name, options) {
    this.dir = dir;
    this.name = name || 'capability-core';
    this.opts = options || {};
    this.threshold = LEVELS[this.opts.level || 'info'];
    this.buffer = [];
    this.maxBuffer = 400;
    if (dir) fs.mkdirSync(dir, { recursive: true });
  }

  write(level, msg, extra) {
    const line = { t: new Date().toISOString(), level, msg: String(msg), ...(extra || {}) };
    this.buffer.push(line);
    if (this.buffer.length > this.maxBuffer) this.buffer.shift();
    if (LEVELS[level] >= this.threshold) {
      const text = line.t + ' [' + level + '] ' + line.msg + (extra ? ' ' + JSON.stringify(extra) : '');
      if (level === 'error') console.error(text); else console.log(text);
    }
    if (this.dir) {
      try {
        const file = path.join(this.dir, this.name + '-' + new Date().toISOString().slice(0, 10) + '.log');
        fs.appendFileSync(file, JSON.stringify(line) + '\n');
      } catch { /* ignore */ }
    }
  }

  debug(m, e) { this.write('debug', m, e); }
  info(m, e) { this.write('info', m, e); }
  warn(m, e) { this.write('warn', m, e); }
  error(m, e) { this.write('error', m, e); }
}

export const logger = new Logger(null, 'capability-core');
export function createLogger(dir, name) { return new Logger(dir, name); }
