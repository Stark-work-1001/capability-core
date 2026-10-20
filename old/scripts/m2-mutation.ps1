$ErrorActionPreference = 'Continue'
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
$root = Split-Path -Parent $PSScriptRoot
$ret  = Join-Path $root "kernel\crates\ac-retrieval\src"
$log  = Join-Path $root "artifacts\m2-mutation.log"
$tmp  = Join-Path $root "artifacts\.mut-tmp.log"

function Run-Case($name, $file, $old, $new) {
    $path = Join-Path $ret $file
    $orig = [System.IO.File]::ReadAllText($path, [System.Text.Encoding]::UTF8)
    if (-not $orig.Contains($old)) {
        Write-Output ("{0,-44} !! 锚点失效（变异未生效）" -f $name)
        return
    }
    $mut = $orig.Replace($old, $new)
    [System.IO.File]::WriteAllText($path, $mut, (New-Object System.Text.UTF8Encoding($false)))
    Push-Location (Join-Path $root "kernel")
    & cargo test --release -p ac-retrieval --lib *>&1 | Out-File -FilePath $tmp -Encoding utf8
    $code = $LASTEXITCODE
    Pop-Location
    [System.IO.File]::WriteAllText($path, $orig, (New-Object System.Text.UTF8Encoding($false)))
    $res = (Get-Content $tmp -Encoding UTF8 | Select-String -Pattern "^test result:" | Select-Object -Last 1).Line
    $verdict = "RED-ok  "
    if ($code -eq 0) { $verdict = "GREEN-BAD" }
    Write-Output ("{0,-44} {1}  {2}" -f $name, $verdict, $res)
}

Remove-Item $log -ErrorAction SilentlyContinue

Run-Case "A 降级时清空 hits" "pipeline.rs" 'Err(e) => (hits, StageState::Failed(format!("全文召回失败：{e}"))),' 'Err(_e) => (Vec::new(), StageState::Failed("x".into())),' 
Run-Case "B 故障却写进 used" "pipeline.rs" "        if ft_state.is_usable() {" "        if true {"
Run-Case "C 精排不排序" "rerank.rs" "    out.sort_by(|a, b| {" "    out.reverse(); let _unused = |a: &Ranked, b: &Ranked| {"
Run-Case "D 增量闸门失效" "fulltext.rs" "Some(_) => unchanged += 1," "Some(_) => { add_doc(&mut writer, id_field, body_field, e).map_err(|er| er.to_string())?; updated += 1; }"
Run-Case "E 不判过期" "fulltext.rs" "if meta.fingerprint != current {" "if false {"
Run-Case "F index_source 返空快照" "store.rs" 'Err("该存储后端未实现 index_source，无法建立全文索引".into())' "Ok(Default::default())"
Run-Case "G &T 不转发 index_source" "store.rs" "    fn index_source(&self) -> Result<IndexSnapshot, String> {`n        (**self).index_source()`n    }" ""
Run-Case "H CJK 不切二元组" "fulltext.rs" "        if is_cjk(run[0].1) {" "        if false {"

Write-Output "== 基线复核 =="
Push-Location (Join-Path $root "kernel")
& cargo test --release -p ac-retrieval --lib *>&1 | Out-File -FilePath $tmp -Encoding utf8
Write-Output ("baseline exit=" + $LASTEXITCODE + "  " + (Get-Content $tmp -Encoding UTF8 | Select-String -Pattern "^test result:" | Select-Object -Last 1).Line)
Pop-Location