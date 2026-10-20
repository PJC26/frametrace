# WinUI Bootstrap Receipt

Date: 2026-09-05  
Status: **UNBLOCKED (user-local SDK)** — thin shell builds

## What changed

1. Installed .NET SDK 8.0.424 to `%LOCALAPPDATA%\Microsoft\dotnet` via `dotnet-install.ps1` (no admin).
2. Installed `Microsoft.WindowsAppSDK.WinUI.CSharp.Templates`.
3. Scaffolded `gui/winui` (FrameTrace.Shell) as a **thin engine host**:
   - Launch `frametrace-app.exe` / `frametrace.exe`
   - Run `make-review`, `make-report`, `qa anomalies` against a case folder
4. `dotnet build -c Release -p:Platform=x64` **succeeded**.

## Still not claimed

- MSIX store packaging / code signing
- Full four-pane forensic GUI (viewer remains generated HTML)
- Auto-bundle of libewf into the shell (FFmpeg is never bundled — guided install or PATH, `docs/PLAN_TOOLS_C_2026-10-20.md`)

## Run

```powershell
$env:Path = "$env:LOCALAPPDATA\Microsoft\dotnet;$env:Path"
cd gui/winui
dotnet run -c Release -p:Platform=x64
```

See `gui/winui/README.md`.
