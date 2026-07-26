param(
  [string]$Server = "http://localhost:5173",
  [string]$Session = ""
)

$Root = Split-Path -Parent $PSScriptRoot
Set-Location "$Root/agent"

if ($Session) {
  cargo run -- -server $Server -session $Session
  exit $LASTEXITCODE
}

cargo run -- -server $Server
exit $LASTEXITCODE
