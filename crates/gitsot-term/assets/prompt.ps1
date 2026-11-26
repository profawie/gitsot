# Gitsot prompt for PowerShell (Windows PowerShell 5.1 and PowerShell 7+).
#
# Loaded only inside Gitsot's built-in terminal, after your own profile.
# Before every prompt it shows one line of labelled chips (folder, branch,
# sync, changes, identity, last command) and then a short prompt. The icons
# are private-use characters that Gitsot's terminal draws itself, so no
# special font is needed.
#
# Turn it off in Gitsot: Settings > Terminal > Gitsot prompt.
#
# Keep this file plain ASCII, with comments on their own lines only.

$global:GitsotPrompt = @{ First = $true; LastId = -1 }

function global:GitsotChip([string]$Rgb, [string]$Text) {
    $e = [char]27
    "${e}[38;2;${Rgb}m$([char]0xE0B6)${e}[48;2;${Rgb}m${e}[1;38;2;255;255;255m${Text}${e}[0m${e}[38;2;${Rgb}m$([char]0xE0B4)${e}[0m "
}

function global:prompt {
    $ok = $?
    $code = $global:LASTEXITCODE
    $e = [char]27
    $state = $global:GitsotPrompt
    $iFolder = [char]0xEE00
    $iBranch = [char]0xEE01
    $iPerson = [char]0xEE02
    $iPencil = [char]0xEE03
    $iClock = [char]0xEE04
    $iCheck = [char]0xEE05
    $iCross = [char]0xEE06
    $iUp = [char]0xEE07
    $iDown = [char]0xEE08
    $iWarn = [char]0xEE09
    $iPrompt = [char]0xEE0A

    $dir = [string]$PWD.ProviderPath
    $shown = $dir
    $sep = [string][IO.Path]::DirectorySeparatorChar
    $repo = [string]$env:GITSOT_REPO
    $repo = $repo.TrimEnd($sep.ToCharArray())
    $userHome = ([string]$HOME).TrimEnd($sep.ToCharArray())
    $ignoreCase = [StringComparison]::OrdinalIgnoreCase
    if ($repo -and ($dir.Equals($repo, $ignoreCase) -or $dir.StartsWith($repo + $sep, $ignoreCase))) {
        $shown = (Split-Path -Leaf $repo) + $dir.Substring($repo.Length)
    } elseif ($userHome -and $dir.StartsWith($userHome + $sep, $ignoreCase)) {
        $shown = '~' + $dir.Substring($userHome.Length)
    }
    $shown = $shown.Replace('\', '/')

    $line = GitsotChip '11;107;168' "$iFolder  $shown"

    $lines = $null
    $gitOk = $false
    try {
        $lines = @(& git --no-optional-locks status --porcelain=v2 --branch 2>$null)
        $gitOk = ($global:LASTEXITCODE -eq 0)
    } catch {
        $gitOk = $false
    }
    if ($gitOk -and $lines) {
        $branch = ''
        $oid = ''
        $upstream = $false
        $ahead = 0
        $behind = 0
        $staged = 0
        $changed = 0
        $new = 0
        $conflicts = 0
        foreach ($item in $lines) {
            $l = [string]$item
            if ($l.StartsWith('# branch.head ')) {
                $branch = $l.Substring(14)
            } elseif ($l.StartsWith('# branch.oid ')) {
                $oid = $l.Substring(13)
            } elseif ($l.StartsWith('# branch.upstream ')) {
                $upstream = $true
            } elseif ($l -match '^# branch\.ab \+(\d+) -(\d+)') {
                $ahead = [int]$Matches[1]
                $behind = [int]$Matches[2]
            } elseif ($l.StartsWith('u ')) {
                $conflicts++
            } elseif ($l.StartsWith('? ')) {
                $new++
            } elseif ($l.Length -gt 4 -and ($l.StartsWith('1 ') -or $l.StartsWith('2 '))) {
                if ($l.Substring(2, 1) -ne '.') { $staged++ }
                if ($l.Substring(3, 1) -ne '.') { $changed++ }
            }
        }

        $detached = ($branch -eq '(detached)')
        if ($detached) {
            $short = $oid
            if ($short.Length -gt 7) { $short = $short.Substring(0, 7) }
            $branch = "detached at $short"
        }
        $line += GitsotChip '15;118;110' "$iBranch  $branch"

        if (-not $upstream) {
            if (-not $detached) { $line += GitsotChip '71;85;105' 'not published' }
        } elseif ($ahead -ne 0 -or $behind -ne 0) {
            $parts = @()
            if ($ahead -ne 0) { $parts += "$iUp  $ahead to push" }
            if ($behind -ne 0) { $parts += "$iDown  $behind to pull" }
            $line += GitsotChip '71;85;105' ($parts -join '  ')
        }

        if ($conflicts -ne 0) {
            $line += GitsotChip '185;28;28' "$iWarn  $conflicts in conflict"
        }
        if ($staged -ne 0 -or $changed -ne 0 -or $new -ne 0) {
            $parts = @()
            if ($staged -ne 0) { $parts += "$staged staged" }
            if ($changed -ne 0) { $parts += "$changed changed" }
            if ($new -ne 0) { $parts += "$new new" }
            $line += GitsotChip '180;83;9' ("$iPencil  " + ($parts -join ', '))
        } elseif ($conflicts -eq 0) {
            $line += GitsotChip '21;128;61' "$iCheck  clean"
        }

        $email = ''
        try {
            $email = [string](& git config user.email 2>$null)
        } catch {
            $email = ''
        }
        if ($email) {
            $line += GitsotChip '109;40;217' "$iPerson  $email"
        } else {
            $line += GitsotChip '185;28;28' "$iPerson  no identity"
        }
    }

    $fresh = $false
    $last = Get-History -Count 1
    if ($last -and $last.Id -ne $state.LastId) {
        $fresh = $true
        $state.LastId = $last.Id
        $took = [int][Math]::Floor(($last.EndExecutionTime - $last.StartExecutionTime).TotalSeconds)
        if ($took -ge 60) {
            $line += GitsotChip '71;85;105' ("$iClock  " + [int][Math]::Floor($took / 60) + 'm ' + ($took % 60) + 's')
        } elseif ($took -ge 3) {
            $line += GitsotChip '71;85;105' "$iClock  ${took}s"
        }
    }
    $failed = ($fresh -and -not $ok)
    if ($failed) {
        if ($code) {
            $line += GitsotChip '185;28;28' "$iCross  exit $code"
        } else {
            $line += GitsotChip '185;28;28' "$iCross  error"
        }
    }

    $lead = "`n"
    if ($state.First) {
        $lead = ''
        $state.First = $false
    }
    $color = '32'
    if ($failed) { $color = '31' }
    $title = "${e}]0;Gitsot: ${shown}$([char]7)"

    $global:LASTEXITCODE = $code
    "${title}${lead}${line}`n${e}[${color}m${iPrompt}${e}[0m "
}
