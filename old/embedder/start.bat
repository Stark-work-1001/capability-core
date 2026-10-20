@echo off
REM ============================================================
REM  capability-core embedder service launcher (Windows side)
REM  Starts the WSL Rocky embedding service on port 18191.
REM  Pure ASCII on purpose (PowerShell/cmd encoding safety).
REM ============================================================
setlocal

set "DEST=/mnt/c/work/capability-core/embedder"

echo ============================================================
echo  capability-core embedder - starting on port 18191
echo ============================================================

REM --- pre-check: is it already up? ---
curl.exe -s -m 3 -o NUL -w "health_precheck_http=%%{http_code}
" http://127.0.0.1:18191/health
curl.exe -s -m 3 http://127.0.0.1:18191/health | findstr /C:""status":"ok"" >NUL
if not errorlevel 1 (
    echo [INFO] Service already running and healthy on 18191.
    goto :done
)

REM --- launch inside WSL (detached, own venv on D:) ---
wsl.exe -d rocky -- bash -lc "mkdir -p %DEST%/logs; nohup %DEST%/venv/bin/python %DEST%/embedder_service.py > %DEST%/logs/embedder.log 2>&1 & echo started_pid=$!"

echo [INFO] Waiting for model load (cold start ~20-40s) ...

REM --- poll health until ready (max 180s) ---
set /a TRIES=0
:wait
set /a TRIES+=1
timeout /t 3 /nobreak >NUL
curl.exe -s -m 3 -o NUL http://127.0.0.1:18191/health
if not errorlevel 1 (
    curl.exe -s -m 3 http://127.0.0.1:18191/health | findstr /C:""status":"ok"" >NUL
    if not errorlevel 1 (
        echo [OK] Service is healthy after %TRIES% polls.
        echo [INFO] Health payload:
        curl.exe -s -m 5 http://127.0.0.1:18191/health
        echo.
        goto :done
    )
)
if %TRIES% lss 60 (
    goto :wait
)

echo [ERROR] Service did not become healthy in time.
echo [INFO] Check log: wsl -d rocky -- bash -lc "tail -40 %DEST%/logs/embedder.log"
exit /b 1

:done
echo ============================================================
echo  Ready: http://127.0.0.1:18191/health
echo          http://127.0.0.1:18191/embed  (POST {"texts":[...]})
echo ============================================================
endlocal
