# Kairos reads which folder a shell is in from Windows, and PowerShell only tells Windows the folder it started in.
# Before every prompt this copies the folder you cd'd to over. kairos uninstall takes these lines out again.
if ("$function:prompt" -notmatch 'KairosPrompt') {
    $global:KairosPrompt = $function:prompt
    function global:prompt {
        $out = & $global:KairosPrompt
        try { [Environment]::CurrentDirectory = $ExecutionContext.SessionState.Path.CurrentFileSystemLocation.ProviderPath } catch {}
        $out
    }
}
