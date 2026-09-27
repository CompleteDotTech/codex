"""Private state directories and fail-closed native Windows ACL inspection."""

import base64
import json
import os
import re

from state_io import ServiceError, run


def private_directory(path):
    path.mkdir(mode=0o700)
    if os.name == "nt":
        # Emit only the ASCII SID; whoami also emits a localized account name.
        sid = run(
            [
                "powershell.exe",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "[System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value",
            ]
        ).strip()
        if not re.fullmatch(r"S-1-[0-9-]+", sid):
            raise ServiceError("windows_identity_unresolved")
        run(
            [
                "icacls",
                str(path),
                "/inheritance:r",
                "/grant:r",
                f"*{sid}:(OI)(CI)F",
                "*S-1-5-18:(OI)(CI)F",
            ],
            discard_output=True,
        )
        _validate_windows_permissions([path])


def _validate_windows_permissions(paths):
    # Read native SIDs/ACEs rather than parsing localized icacls output. Paths are
    # passed as data, never interpolated into executable PowerShell text.
    script = """
$ErrorActionPreference = 'Stop'
$trusted = @([System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value,
             'S-1-5-18', 'S-1-5-32-544')
# OWNER RIGHTS grants apply only to the trusted owner verified below.
$grantees = $trusted + 'S-1-3-4'
foreach ($path in (ConvertFrom-Json $env:CODEX_PG_ACL_PATHS)) {
    $attributes = [System.IO.File]::GetAttributes($path)
    if ($attributes -band [System.IO.FileAttributes]::ReparsePoint) { exit 1 }
    if ($attributes -band [System.IO.FileAttributes]::Directory) {
        $acl = [System.IO.Directory]::GetAccessControl($path)
    } else {
        $acl = [System.IO.File]::GetAccessControl($path)
    }
    $descriptor = [System.Security.AccessControl.RawSecurityDescriptor]::new(
        $acl.GetSecurityDescriptorBinaryForm(), 0)
    if ($null -eq $descriptor.DiscretionaryAcl -or
        $descriptor.Owner.Value -notin $trusted) { exit 1 }
    foreach ($ace in $descriptor.DiscretionaryAcl) {
        if ($ace -isnot [System.Security.AccessControl.CommonAce] -or
            $ace.IsCallback) { exit 1 }
        if ($ace.AceQualifier -eq 'AccessAllowed' -and $ace.AccessMask -ne 0 -and
            $ace.SecurityIdentifier.Value -notin $grantees) { exit 1 }
        if ($ace.AceQualifier -notin @('AccessAllowed', 'AccessDenied')) { exit 1 }
    }
}
"""
    encoded = base64.b64encode(script.encode("utf-16le")).decode("ascii")
    env = dict(os.environ, CODEX_PG_ACL_PATHS=json.dumps([str(path) for path in paths]))
    try:
        run(
            [
                "powershell.exe",
                "-NoProfile",
                "-NonInteractive",
                "-EncodedCommand",
                encoded,
            ],
            env=env,
            discard_output=True,
        )
    except ServiceError:
        raise ServiceError(
            "insecure_or_unverifiable_windows_state_permissions"
        ) from None
