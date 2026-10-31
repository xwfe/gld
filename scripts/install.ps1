# Downloads a gld release, checks its checksum and installs it for this user.
# Needs no administrator rights and does not touch gld's data or node configuration.
#
#   $env:GLD_VERSION='v0.10.0'; irm https://github.com/xwfe/gld/releases/download/v0.10.0/install.ps1 | iex
#
# Environment variables:
#   GLD_VERSION      version to install, such as v0.10.0; defaults to the latest release
#   GLD_INSTALL_DIR  where to install; defaults to %LOCALAPPDATA%\Programs\gld
#   GLD_BASE_URL     download prefix; defaults to the GitHub release, for internal mirrors
#
# Keep this file ASCII. Windows PowerShell 5.1 decodes a downloaded script with the wrong code page, and
# bytes of UTF-8 Chinese text can come out as curly quotes, which PowerShell parses as real quotes.
# It runs through iex, so it must never call exit: that would close the user's window. Failures throw.
# It must work on the 5.1 that ships with Windows: TLS 1.2 is off by default there, and
# Invoke-WebRequest needs -UseBasicParsing.
& {
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    function Fail($message) { throw "gld install failed: $message" }

    # Windows on ARM runs x64 programs, so this package covers it too.
    $target = 'x86_64-pc-windows-msvc'

    $version = $env:GLD_VERSION
    if (-not $version) {
        try {
            $version = (Invoke-RestMethod 'https://api.github.com/repos/xwfe/gld/releases/latest').tag_name
        } catch {
            Fail 'cannot find the latest version. Can this machine reach GitHub? You can also set GLD_VERSION.'
        }
    }
    if (-not $version.StartsWith('v')) { $version = "v$version" }

    $base = $env:GLD_BASE_URL
    if (-not $base) { $base = "https://github.com/xwfe/gld/releases/download/$version" }
    $archive = "gld-$version-$target.zip"
    $installDir = $env:GLD_INSTALL_DIR
    if (-not $installDir) { $installDir = Join-Path $env:LOCALAPPDATA 'Programs\gld' }

    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("gld-install-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        Write-Host "Downloading $archive ..."
        $archivePath = Join-Path $tmp $archive
        $sumsPath = Join-Path $tmp 'SHA256SUMS'
        try {
            Invoke-WebRequest -UseBasicParsing -Uri "$base/$archive" -OutFile $archivePath
        } catch {
            Fail "cannot download $base/$archive. Check that version $version exists and that this machine can reach GitHub."
        }
        try {
            Invoke-WebRequest -UseBasicParsing -Uri "$base/SHA256SUMS" -OutFile $sumsPath
        } catch {
            Fail "cannot download $base/SHA256SUMS, so the package cannot be checked. Nothing was installed."
        }

        $expected = $null
        foreach ($line in Get-Content $sumsPath) {
            $parts = $line -split '\s+'
            if ($parts.Count -ge 2 -and $parts[1] -eq $archive) { $expected = $parts[0] }
        }
        if (-not $expected) { Fail "SHA256SUMS does not list $archive. Nothing was installed." }
        $actual = (Get-FileHash -Algorithm SHA256 $archivePath).Hash.ToLower()
        if ($actual -ne $expected.ToLower()) {
            Fail "checksum mismatch for $archive (expected $expected, got $actual). The download may be altered or incomplete. Nothing was installed."
        }

        Expand-Archive -Path $archivePath -DestinationPath $tmp
        $binary = Join-Path $tmp "gld-$version-$target\gld.exe"
        if (-not (Test-Path $binary)) { Fail "$archive does not contain gld.exe. Nothing was installed." }
        # Run it once first: a binary that cannot run here must not replace one that can.
        $reported = & $binary --version 2>&1
        if ($LASTEXITCODE -ne 0) { Fail "the downloaded gld cannot run on this machine: $reported. Nothing was installed." }

        New-Item -ItemType Directory -Force -Path $installDir | Out-Null
        $installed = Join-Path $installDir 'gld.exe'
        $staged = Join-Path $installDir ".gld.installing.$PID.exe"
        Copy-Item $binary $staged -Force
        # A running exe cannot be overwritten but can be renamed, so move the old one aside. A node may still
        # be running from an earlier .old file that cannot be deleted yet, so each one gets its own name.
        $old = "$installed.old.$PID"
        Remove-Item "$installed.old.*" -Force -ErrorAction SilentlyContinue
        $replaced = Test-Path $installed
        try {
            if ($replaced) { Move-Item $installed $old -Force }
            try {
                Move-Item $staged $installed -Force
            } catch {
                if ($replaced) { Move-Item $old $installed -Force }
                throw
            }
        } finally {
            Remove-Item $staged -Force -ErrorAction SilentlyContinue
        }
        if ($replaced) { Remove-Item $old -Force -ErrorAction SilentlyContinue }

        Write-Host "Installed $reported to $installed"
        if ($replaced) { Write-Host "Replaced the previous version. If a node runs in the background, run: gld node autostart enable" }

        # The user-level Path needs no administrator rights. Update this window too so it works right away.
        # Go through the registry: [Environment] expands %VAR% entries and writes them back flattened.
        $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey('Environment')
        $userPath = $key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
        if (-not (($userPath -split ';') -contains $installDir)) {
            $newPath = if ($userPath) { "$userPath;$installDir" } else { $installDir }
            $key.SetValue('Path', $newPath, [Microsoft.Win32.RegistryValueKind]::ExpandString)
            # Setting any variable this way makes Windows tell open programs that the environment changed.
            [Environment]::SetEnvironmentVariable('GLD_INSTALL_PATH_NOTICE', $null, 'User')
            Write-Host "Added $installDir to the user Path."
        }
        $key.Close()
        if (-not (($env:Path -split ';') -contains $installDir)) { $env:Path = "$env:Path;$installDir" }
    } finally {
        Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
}
