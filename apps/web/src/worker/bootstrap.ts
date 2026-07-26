type BootstrapOptions = {
  binaryBaseURL: string;
  checksumsURL: string;
  serverOrigin: string;
  sessionId?: string | null;
};

export function renderShellBootstrap({
  binaryBaseURL,
  checksumsURL,
  serverOrigin,
  sessionId,
}: BootstrapOptions) {
  return `#!/bin/sh
set -eu

SERVER_URL="${serverOrigin}"
BINARY_BASE_URL="${binaryBaseURL}"
CHECKSUMS_URL="${checksumsURL}"
SESSION_ID="${sessionId ?? ""}"
if [ -z "$SESSION_ID" ]; then
  SESSION_ID="\${1:-}"
fi
TMP_DIR="\${TMPDIR:-/tmp}/ttys"
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

case "$ARCH" in
  x86_64|amd64) ARCH="amd64" ;;
  arm64|aarch64) ARCH="arm64" ;;
  *)
    echo "unsupported architecture: $ARCH" >&2
    exit 1
    ;;
esac

case "$OS" in
  darwin|linux) ;;
  *)
    echo "unsupported operating system: $OS" >&2
    exit 1
    ;;
esac

mkdir -p "$TMP_DIR"
ASSET_NAME="ttys-agent-$OS-$ARCH"
AGENT_PATH="$TMP_DIR/$ASSET_NAME"
CHECKSUMS_PATH="$TMP_DIR/ttys-agent-checksums.txt"

if command -v curl >/dev/null 2>&1; then
  download() {
    curl -fsSL --compressed "$1" -o "$2"
  }
elif command -v wget >/dev/null 2>&1; then
  download() {
    wget -qO "$2" "$1"
  }
else
  echo "curl or wget is required to download ttys-agent" >&2
  exit 1
fi

download "$CHECKSUMS_URL" "$CHECKSUMS_PATH"

download_and_verify() {
  asset_name="$1"
  agent_path="$2"
  download "$BINARY_BASE_URL/$asset_name" "$agent_path"

  expected_checksum="$(awk -v asset="$asset_name" '$2 == asset { print $1; exit }' "$CHECKSUMS_PATH")"
  if [ -z "$expected_checksum" ]; then
    echo "missing checksum for $asset_name" >&2
    exit 1
  fi

  if command -v shasum >/dev/null 2>&1; then
    actual_checksum="$(shasum -a 256 "$agent_path" | awk '{print $1}')"
  elif command -v sha256sum >/dev/null 2>&1; then
    actual_checksum="$(sha256sum "$agent_path" | awk '{print $1}')"
  else
    echo "shasum or sha256sum is required to verify ttys-agent" >&2
    exit 1
  fi

  if [ "$actual_checksum" != "$expected_checksum" ]; then
    echo "checksum mismatch for $asset_name" >&2
    exit 1
  fi

  chmod +x "$agent_path"
}

download_and_verify "$ASSET_NAME" "$AGENT_PATH"

if [ "$OS" = "linux" ] && ! "$AGENT_PATH" --version >/dev/null 2>&1; then
  PORTABLE_ASSET_NAME="$ASSET_NAME-portable"
  PORTABLE_AGENT_PATH="$TMP_DIR/$PORTABLE_ASSET_NAME"
  echo "ttys-agent: system TLS is unavailable; using the portable TLS fallback." >&2
  download_and_verify "$PORTABLE_ASSET_NAME" "$PORTABLE_AGENT_PATH"
  if ! "$PORTABLE_AGENT_PATH" --version >/dev/null 2>&1; then
    echo "ttys-agent portable fallback could not start" >&2
    exit 1
  fi
  ASSET_NAME="$PORTABLE_ASSET_NAME"
  AGENT_PATH="$PORTABLE_AGENT_PATH"
fi

TTY_DEVICE="/dev/tty"
if [ ! -r "$TTY_DEVICE" ] || [ ! -w "$TTY_DEVICE" ]; then
  echo "ttys-agent requires an interactive terminal (/dev/tty not available)" >&2
  exit 1
fi

if [ -n "$SESSION_ID" ]; then
  exec "$AGENT_PATH" -server "$SERVER_URL" -session "$SESSION_ID" <"$TTY_DEVICE" >"$TTY_DEVICE" 2>"$TTY_DEVICE"
fi

exec "$AGENT_PATH" -server "$SERVER_URL" <"$TTY_DEVICE" >"$TTY_DEVICE" 2>"$TTY_DEVICE"
`;
}

export function renderPowerShellBootstrap({
  binaryBaseURL,
  checksumsURL,
  serverOrigin,
  sessionId,
}: BootstrapOptions) {
  return `param(
  [string]$Session = "${sessionId ?? ""}"
)

$Server = "${serverOrigin}"
$BinaryBaseUrl = "${binaryBaseURL}"
$ChecksumsUrl = "${checksumsURL}"
$Os = "windows"

switch ($env:PROCESSOR_ARCHITECTURE.ToLower()) {
  "amd64" { $Arch = "amd64" }
  "arm64" { $Arch = "arm64" }
  default {
    Write-Error "unsupported architecture: $env:PROCESSOR_ARCHITECTURE"
    exit 1
  }
}

$Tmp = Join-Path ([System.IO.Path]::GetTempPath()) "ttys"
New-Item -ItemType Directory -Force -Path $Tmp | Out-Null
$AssetName = "ttys-agent-$Os-$Arch.exe"
$AgentPath = Join-Path $Tmp $AssetName
$DownloadUrl = "$BinaryBaseUrl/$AssetName"
$ChecksumsPath = Join-Path $Tmp "ttys-agent-checksums.txt"

$HttpHandler = [System.Net.Http.HttpClientHandler]::new()
$HttpHandler.AutomaticDecompression = [System.Net.DecompressionMethods]::GZip -bor [System.Net.DecompressionMethods]::Deflate
$HttpClient = [System.Net.Http.HttpClient]::new($HttpHandler)
try {
  $AgentResponse = $HttpClient.GetAsync($DownloadUrl).GetAwaiter().GetResult()
  try {
    $AgentResponse.EnsureSuccessStatusCode() | Out-Null
    $AgentBytes = $AgentResponse.Content.ReadAsByteArrayAsync().GetAwaiter().GetResult()
  } finally {
    $AgentResponse.Dispose()
  }
  [System.IO.File]::WriteAllBytes($AgentPath, $AgentBytes)
} finally {
  $HttpClient.Dispose()
  $HttpHandler.Dispose()
}
Invoke-WebRequest -UseBasicParsing -Uri $ChecksumsUrl -OutFile $ChecksumsPath

$ExpectedChecksum = $null
foreach ($line in Get-Content $ChecksumsPath) {
  if ($line -match "^(?<hash>[0-9a-fA-F]+)\\s{2}(?<name>.+)$" -and $Matches["name"] -eq $AssetName) {
    $ExpectedChecksum = $Matches["hash"].ToLower()
    break
  }
}

if (-not $ExpectedChecksum) {
  Write-Error "missing checksum for $AssetName"
  exit 1
}

$ActualChecksum = (Get-FileHash -Algorithm SHA256 $AgentPath).Hash.ToLower()
if ($ActualChecksum -ne $ExpectedChecksum) {
  Write-Error "checksum mismatch for $AssetName"
  exit 1
}

$AgentArgs = @("-server", $Server)
if ($Session) {
  $AgentArgs += @("-session", $Session)
}

$AgentProcess = Start-Process -FilePath $AgentPath -ArgumentList $AgentArgs -NoNewWindow -Wait -PassThru
exit $AgentProcess.ExitCode
`;
}
