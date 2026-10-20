import React, { useEffect, useState } from 'react';
import { api, HOST_LABEL, type StatusInfo } from '../lib/api';
import { Badge } from '../components/ui';

export default function SettingsView({ status, toast, onChanged }: { status: StatusInfo | null; toast: (m: string, t?: string) => void; onChanged: () => void }) {
  const [settings, setSettings] = useState<Record<string, any>>({});
  const [server, setServer] = useState<any>(null);
  const [trash, setTrash] = useState<any[]>([]);

  const loadTrash = () => api.trash().then(setTrash).catch(() => {});
  useEffect(() => {
    api.settings().then(setSettings).catch(() => {});
    api.serverInfo().then(setServer).catch(() => {});
    loadTrash();
  }, []);

  const save = async (patch: any) => {
    try { const s = await api.saveSettings(patch); setSettings(s); toast('已保存', 'success'); }
    catch (e: any) { toast(e.message, 'error'); }
  };

  const restore = async (id: string) => {
    try { await api.capabilityAction(id, 'restore'); toast('已恢复到原路径', 'success'); loadTrash(); onChanged(); }
    catch (e: any) { toast(e.message, 'error'); }
  };

  const mcpSample = JSON.stringify({ mcpServers: { 'capability-core': { type: 'streamableHttp', url: 'http://127.0.0.1:' + (server?.port || status?.port || 18297) + '/mcp' } } }, null, 2);
  const tools = [
    ['capability.search', '按自然语言检索能力'],
    ['capability.list', '分页浏览能力'],
    ['capability.get', '读取能力详情与安装路径'],
    ['capability.read_skill', '读取技能正文'],
    ['capability.report_usage', '上报调用记录'],
    ['capability.list_faults', '查询故障工单'],
    ['capability.submit_patch', '回传维修补丁'],
    ['capability.register_agent', '登记智能体'],
    ['capability.registry_stats', '中枢统计'],
    ['capability.sync_status', '同步状态'],
    ['capability.assets_list', '列出能力资产库条目'],
    ['capability.assets_read', '读取单个能力资产内容'],
    ['capability.model_status', '查询模型接入可用性（不含密钥）']
  ];

  return (
    <div className="grid cols-2">
      <div>
        <div className="card mb16">
          <div className="card-header"><div className="card-title">运行信息</div></div>
          <div className="card-body">
            <dl className="kv">
              <dt>版本</dt><dd className="mono">v{status?.version} {status?.dev ? '（开发模式）' : ''}</dd>
              <dt>数据目录</dt><dd className="mono hint">{status?.home}</dd>
              <dt>数据库</dt><dd className="mono hint">{status?.dbPath}</dd>
              <dt>MCP 端点</dt><dd className="mono">http://127.0.0.1:{server?.port || status?.port}/mcp</dd>
              <dt>服务状态</dt><dd>{status?.serverRunning ? <Badge tone="green">运行中</Badge> : <Badge tone="red">未运行</Badge>}</dd>
            </dl>
            <div className="flex mt12">
              <button className="btn small" onClick={() => api.openPath(status?.home || '')}>打开数据目录</button>
              <button className="btn small" onClick={async () => { try { const r = await api.restartServer(); toast('服务已重启，端口 ' + r.port, 'success'); } catch (e: any) { toast(e.message, 'error'); } }}>重启服务</button>
            </div>
          </div>
        </div>

        <div className="card mb16">
          <div className="card-header"><div className="card-title">自动化</div></div>
          <div className="card-body">
            {[
              ['scanOnStart', '启动时自动扫描', '应用启动后后台扫描全部来源'],
              ['autoScan', '定时增量扫描', '每 15 分钟刷新一次宿主能力库'],
              ['diagnoseOnScan', '扫描后自动诊断', '扫描完成立即执行完整性检查']
            ].map(([key, label, hint]) => (
              <div key={key} className="flex" style={{ padding: '9px 0', borderBottom: '1px solid #131e27' }}>
                <div className="grow">
                  <div style={{ fontSize: 12.5 }}>{label}</div>
                  <div className="hint">{hint}</div>
                </div>
                <input type="checkbox" checked={settings[key] !== false} onChange={(e) => save({ [key]: e.target.checked })} />
              </div>
            ))}
          </div>
        </div>

        <div className="card">
          <div className="card-header"><div className="card-title">回收站</div><div className="grow" /><span className="hint">{trash.length}</span></div>
          <div className="card-body" style={{ padding: 0, maxHeight: 280, overflowY: 'auto' }}>
            {trash.map((t) => (
              <div key={t.id} className="list-item" style={{ cursor: 'default' }}>
                <div className="title">{t.name}<div className="grow" /><Badge tone="gray">{t.kind}</Badge></div>
                <div className="meta"><span>{(t.files || []).length} 个文件已移出</span>
                  <button className="btn small ghost" onClick={() => restore(t.id)}>恢复</button></div>
              </div>
            ))}
            {!trash.length ? <div className="empty hint">回收站为空</div> : null}
          </div>
        </div>
      </div>

      <div>
        <div className="card mb16">
          <div className="card-header"><div className="card-title">智能体接入</div></div>
          <div className="card-body">
            <div className="hint mb12">任何支持 MCP 的智能体都可以连入。把下面的配置写入宿主，或到「生态同步」一键写入（自动备份）。</div>
            <div className="code" style={{ fontSize: 11 }}>{mcpSample}</div>
            <div className="hint mt8">只支持 stdio 的宿主：node apps/desktop/src/main/cli.js stdio</div>
            <div className="section-title">可用工具</div>
            <table className="table">
              <tbody>
                {tools.map(([n, d]) => (
                  <tr key={n} style={{ cursor: 'default' }}><td className="mono" style={{ width: 190, color: '#7fe6c7' }}>{n}</td><td className="hint">{d}</td></tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>

        <div className="card">
          <div className="card-header"><div className="card-title">关于</div></div>
          <div className="card-body">
            <div className="hint" style={{ lineHeight: 1.95 }}>
              <b>能力核心 Capability Core</b> 是跨智能体的能力资产中枢：把分散在各宿主中的技能、MCP 服务与插件统一采集进本地数据库，
              通过 MCP 协议向任何智能体开放检索、读取与治理能力，并承担故障诊断、重复裁决与更新巡检。<br /><br />
              <b>能力资产库</b>（自有，assets/）与<b>宿主能力库</b>（扫描登记）共用一套元数据与检索链路，
              宿主能力可一键「转为能力资产」，能力资产也可「生态同步」写回宿主原生配置。<br /><br />
              模型接入为云端增强项：向量嵌入、精排与判定可走外部提供商，云端不可用时自动退回本地
              FTS5 + trigram + BM25 与规则引擎，任何路径都不会中断。<br /><br />
              数据全部留在本机，默认只监听 127.0.0.1；写操作均有备份与回滚路径。
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
