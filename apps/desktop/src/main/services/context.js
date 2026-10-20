import { openDatabase } from '../db/db.js';
import { EventBus } from '../core/events.js';
import { createLogger } from '../core/logger.js';
import { Registry } from './registry.js';
import { Scanner } from './scanner.js';
import { DedupeService } from './dedupe.js';
import { SearchService } from './search.js';
import { UpdateService } from './updates.js';
import { EcosystemService } from './ecosystem.js';
import { AgentService } from './agents.js';
import { FaultService } from './faults.js';
import { WorkflowService } from './workflows.js';
import { LifecycleService } from './lifecycle.js';
import { ImportService } from './imports.js';
import { AssetService } from './assets.js';
import { ModelService } from './models.js';
import { DB_PATH, LOG_DIR, HOME, BACKUP_DIR, EXPORT_DIR, RUN_DIR, ASSET_DIR, ensureDirs } from '../core/paths.js';

// 组合根：把 db / 事件总线 / 日志 / 服务组装起来，供 Electron 主进程、HTTP 服务、MCP stdio 复用。
// 所有入口（GUI、CLI、MCP）共用同一套服务，避免行为分叉。
export function createContext(options) {
  const opts = options || {};
  ensureDirs();
  const db = openDatabase(opts.dbPath || DB_PATH, opts);
  const bus = new EventBus(db);
  const logger = createLogger(opts.logDir === null ? null : (opts.logDir || LOG_DIR), 'capability-core');
  const assetDir = opts.assetDir || ASSET_DIR;
  const ctx = {
    db, bus, logger,
    options: { home: HOME, assetDir, backupDir: BACKUP_DIR, exportDir: EXPORT_DIR, runDir: RUN_DIR, ...opts }
  };
  // 调用方传入 assetDir 时以调用方为准（测试/多实例隔离）
  ctx.options.assetDir = assetDir;
  const services = {};
  ctx.services = services;
  services.registry = new Registry(ctx);
  services.scanner = new Scanner(ctx);
  services.dedupe = new DedupeService(ctx);
  services.search = new SearchService(ctx);
  services.updates = new UpdateService(ctx);
  services.ecosystem = new EcosystemService(ctx);
  services.agents = new AgentService(ctx);
  services.faults = new FaultService(ctx);
  services.workflows = new WorkflowService(ctx, services);
  services.lifecycle = new LifecycleService(ctx, services);
  services.imports = new ImportService(ctx, services);
  // 新增：能力资产库（自有资产）与模型接入（云端能力 + 本地兜底）
  services.assets = new AssetService(ctx, services);
  services.models = new ModelService(ctx);
  // 兼容别名：内部服务间既有调用
  ctx.registry = services.registry;
  ctx.scanner = services.scanner;
  return ctx;
}

export function closeContext(ctx) {
  if (!ctx) return;
  try { ctx.db.close(); } catch { /* ignore */ }
}
