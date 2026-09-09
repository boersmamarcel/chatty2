# Locate fxc.exe (the DirectX shader compiler) for gpui's build script.
#
# gpui compiles its HLSL shaders at build time on Windows release builds and
# panics with "Failed to find fxc.exe" unless the compiler is on PATH, at one
# hard-coded Windows Kits path, or named by GPUI_FXC_PATH. Export the newest
# SDK's x64 copy so the desktop build does not depend on the runner image's
# SDK layout. Used by ci.yml (warm-release-cache) and release.yml.
$ErrorActionPreference = "Stop"

$kits = Join-Path ${env:ProgramFiles(x86)} "Windows Kits\10\bin"
$fxc = Get-ChildItem -Path $kits -Filter fxc.exe -Recurse -ErrorAction SilentlyContinue |
    Where-Object { $_.Directory.Name -eq "x64" } |
    Sort-Object {
        $v = $null
        if ([version]::TryParse($_.Directory.Parent.Name, [ref]$v)) { $v } else { [version]"0.0" }
    } -Descending |
    Select-Object -First 1

if (-not $fxc) {
    Write-Error "fxc.exe not found under $kits - install a Windows 10/11 SDK"
    exit 1
}

Write-Host "Using fxc.exe at $($fxc.FullName)"
"GPUI_FXC_PATH=$($fxc.FullName)" | Out-File -FilePath $env:GITHUB_ENV -Append -Encoding utf8
