//! 只读体检：确认 M2 切片**没有**在用户的真实库上造成解析器重跑。
//! 以只读方式打开（SQLite URI mode=ro），任何写尝试都会直接失败。
use rusqlite::Connection;

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "../data/registry.db".into());
    let uri = format!("file:{}?mode=ro", path.replace('\\', "/"));
    let conn = Connection::open(&uri).expect("只读打开失败");

    let q = |sql: &str| -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).unwrap_or(-1)
    };

    println!("库: {path}");
    println!("user_version          = {}", q("PRAGMA user_version"));
    println!("assets 总行           = {}", q("SELECT COUNT(*) FROM assets"));
    println!("logical_assets       = {}", q("SELECT COUNT(*) FROM logical_assets"));
    println!("asset_locations      = {}", q("SELECT COUNT(*) FROM asset_locations"));
    println!("normalized_hash 非空  = {}", q("SELECT COUNT(*) FROM logical_assets WHERE normalized_hash IS NOT NULL AND normalized_hash <> ''"));
    println!("distinct content_hash = {}", q("SELECT COUNT(DISTINCT content_hash) FROM assets WHERE content_hash IS NOT NULL AND content_hash <> ''"));
    println!("SKILL.md 真实 version 数 = {}", q("SELECT COUNT(*) FROM assets WHERE version <> '1' AND version <> '0'"));
    println!("已裁决(merged/kb)      = {}", q("SELECT COUNT(*) FROM asset_locations WHERE role IN ('merged')"));

    let revs: Vec<(i64, i64)> = {
        let mut st = conn.prepare("SELECT parser_rev, COUNT(*) FROM assets GROUP BY parser_rev").unwrap();
        let v = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        v.collect::<Result<Vec<_>, _>>().unwrap()
    };
    println!("parser_rev 分布       = {revs:?}");

    let roles: Vec<(String, i64)> = {
        let mut st = conn.prepare("SELECT role, COUNT(*) FROM asset_locations GROUP BY role").unwrap();
        let v = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        v.collect::<Result<Vec<_>, _>>().unwrap()
    };
    println!("location role 分布    = {roles:?}");

    // PARSER_REV 必须与 store.rs 里的常量一致；不一致就意味着下次扫描会全量重写。
    let want: i64 = ac_registry::store::PARSER_REV;
    println!("代码里的 PARSER_REV   = {want}");
    let max_rev = q("SELECT IFNULL(MAX(parser_rev), 0) FROM assets");
    println!("库内 MAX(parser_rev)  = {max_rev}");
    if max_rev != want {
        println!("!! 不一致：下次扫描会把全部 {} 行判成 updated（应考虑 +1 PARSER_REV）", q("SELECT COUNT(*) FROM assets"));
    } else {
        println!("OK：库内 parser_rev 与代码一致，不会发生全量重写");
    }
}
