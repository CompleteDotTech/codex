"""Native protected Windows security descriptors and handle-based inspection."""

import contextlib
import ctypes
from ctypes import wintypes as W

from state_io import ServiceError

# Restrict DLL loading to System32, independent of the caller's current directory.
kernel32 = ctypes.WinDLL("kernel32.dll", use_last_error=True, winmode=0x800)
advapi32 = ctypes.WinDLL("advapi32.dll", use_last_error=True, winmode=0x800)
P = ctypes.c_void_p


def bind(library, name, result, *arguments):
    function = getattr(library, name)
    function.restype = result
    function.argtypes = list(arguments)
    return function


close_handle = bind(kernel32, "CloseHandle", W.BOOL, W.HANDLE)
local_free = bind(kernel32, "LocalFree", P, P)
current_process = bind(kernel32, "GetCurrentProcess", W.HANDLE)
open_token = bind(advapi32, "OpenProcessToken", W.BOOL, W.HANDLE, W.DWORD, P)
token_info = bind(
    advapi32, "GetTokenInformation", W.BOOL, W.HANDLE, W.DWORD, P, W.DWORD, P
)
sid_string = bind(advapi32, "ConvertSidToStringSidW", W.BOOL, P, P)
convert_descriptor = bind(
    advapi32,
    "ConvertStringSecurityDescriptorToSecurityDescriptorW",
    W.BOOL,
    W.LPCWSTR,
    W.DWORD,
    P,
    P,
)
get_security = bind(
    advapi32,
    "GetSecurityInfo",
    W.DWORD,
    W.HANDLE,
    W.DWORD,
    W.DWORD,
    P,
    P,
    P,
    P,
    P,
)
get_control = bind(advapi32, "GetSecurityDescriptorControl", W.BOOL, P, P, P)
get_ace = bind(advapi32, "GetAce", W.BOOL, P, W.DWORD, P)
valid_acl = bind(advapi32, "IsValidAcl", W.BOOL, P)


class SecurityAttributes(ctypes.Structure):
    _fields_ = [("length", W.DWORD), ("descriptor", P), ("inherit", W.BOOL)]


class Acl(ctypes.Structure):
    _fields_ = [
        ("revision", W.BYTE),
        ("reserved", W.BYTE),
        ("size", W.WORD),
        ("count", W.WORD),
        ("reserved2", W.WORD),
    ]


class Ace(ctypes.Structure):
    _fields_ = [
        ("kind", W.BYTE),
        ("flags", W.BYTE),
        ("size", W.WORD),
        ("mask", W.DWORD),
    ]


def checked(success):
    if not success:
        raise ctypes.WinError(ctypes.get_last_error())


def string_sid(pointer):
    encoded = P()
    checked(sid_string(pointer, ctypes.byref(encoded)))
    try:
        return ctypes.wstring_at(encoded)
    finally:
        local_free(encoded)


def current_sid():
    token = W.HANDLE()
    checked(open_token(current_process(), 0x8, ctypes.byref(token)))  # TOKEN_QUERY
    try:
        length = W.DWORD()
        token_info(token, 1, None, 0, ctypes.byref(length))  # TokenUser
        if not 0 < length.value <= 65536:
            raise ServiceError("windows_identity_unresolved")
        data = ctypes.create_string_buffer(length.value)
        checked(token_info(token, 1, data, length, ctypes.byref(length)))
        return string_sid(P.from_buffer(data))
    finally:
        close_handle(token)


@contextlib.contextmanager
def private_attributes():
    """Supply an explicit owner and protected DACL at the creation operation."""
    sid = current_sid()
    descriptor = P()
    checked(
        convert_descriptor(
            f"O:{sid}D:P(A;OICI;FA;;;{sid})(A;OICI;FA;;;SY)",
            1,
            ctypes.byref(descriptor),
            None,
        )
    )
    try:
        yield SecurityAttributes(ctypes.sizeof(SecurityAttributes), descriptor, False)
    finally:
        local_free(descriptor)


def validate_handle(handle):
    """Accept only a trusted owner and explicit protected, non-NULL private DACL."""
    owner, dacl, descriptor = P(), P(), P()
    error = get_security(
        handle,
        1,
        0x1 | 0x4,
        ctypes.byref(owner),
        None,
        ctypes.byref(dacl),
        None,
        ctypes.byref(descriptor),
    )
    if error:
        raise ctypes.WinError(error)
    try:
        control, revision = W.WORD(), W.DWORD()
        checked(get_control(descriptor, ctypes.byref(control), ctypes.byref(revision)))
        trusted = {current_sid(), "S-1-5-18", "S-1-5-32-544"}
        if (
            not owner
            or string_sid(owner) not in trusted
            or not control.value & 0x1000  # SE_DACL_PROTECTED
            or not dacl
            or not valid_acl(dacl)
        ):
            raise ServiceError("insecure_or_unverifiable_windows_state_permissions")
        grantees = trusted | {"S-1-3-4"}  # OWNER RIGHTS applies to the trusted owner.
        for index in range(Acl.from_address(dacl.value).count):
            pointer = P()
            checked(get_ace(dacl, index, ctypes.byref(pointer)))
            ace = Ace.from_address(pointer.value)
            if ace.kind not in (0, 1) or ace.flags & 0x10 or ace.size < 16:
                raise ServiceError("insecure_or_unverifiable_windows_state_permissions")
            if (
                ace.kind == 0
                and ace.mask
                and string_sid(P(pointer.value + ctypes.sizeof(Ace))) not in grantees
            ):
                raise ServiceError("insecure_or_unverifiable_windows_state_permissions")
    finally:
        local_free(descriptor)
