param([switch]$Test, [switch]$Benchmark)
$ErrorActionPreference = 'Stop'
Set-Location -LiteralPath $PSScriptRoot
$taskCargo = Get-Command cargo -ErrorAction SilentlyContinue
if (Test-Path -LiteralPath (Join-Path $PSScriptRoot '.tools\cargo\bin\cargo.exe')) {
    $env:CARGO_HOME = Join-Path $PSScriptRoot '.tools\cargo'
    $env:RUSTUP_HOME = Join-Path $PSScriptRoot '.tools\rustup'
    $taskCargo = Join-Path $env:CARGO_HOME 'bin\cargo.exe'
} elseif ($taskCargo) { $taskCargo = $taskCargo.Source }
else { throw 'Install stable Rust from https://rust-lang.org/tools/install, then run build.ps1 again.' }
if ($Test) { & $taskCargo test --locked; if ($LASTEXITCODE -ne 0) { throw 'Tests failed.' } }
& $taskCargo build --release --locked
if ($LASTEXITCODE -ne 0) { throw 'Build failed.' }
New-Item -ItemType Directory -Force -Path (Join-Path $PSScriptRoot 'dist') | Out-Null
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'target\release\just-a-markdown-editor.exe') -Destination (Join-Path $PSScriptRoot 'dist\Just a markdown editor.exe')
if ($Benchmark) { & (Join-Path $PSScriptRoot 'target\release\performance-check.exe'); if ($LASTEXITCODE -ne 0) { throw 'Performance gate failed. See performance-results.json.' } }
