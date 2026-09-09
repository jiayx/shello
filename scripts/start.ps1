param(
  [string]$Server = "http://localhost:5173",
  [string]$Session = ""
)

$Root = Split-Path -Parent $PSScriptRoot
$Manifest = Join-Path $Root "agent/Cargo.toml"

if ($Session) {
  cargo run --manifest-path $Manifest -- -server $Server -session $Session
  exit $LASTEXITCODE
}

cargo run --manifest-path $Manifest -- -server $Server
exit $LASTEXITCODE
