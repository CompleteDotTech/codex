"""Open one Windows child relative to a retained parent, refusing reparses."""

import ctypes

from state_io import ServiceError
from windows_acl import P, W, bind

ntdll = ctypes.WinDLL("ntdll.dll", use_last_error=True, winmode=0x800)
nt_create = bind(
    ntdll,
    "NtCreateFile",
    W.LONG,
    P,
    W.DWORD,
    P,
    P,
    P,
    W.DWORD,
    W.DWORD,
    W.DWORD,
    W.DWORD,
    P,
    W.DWORD,
)
dos_error = bind(ntdll, "RtlNtStatusToDosError", W.DWORD, W.LONG)


class UnicodeName(ctypes.Structure):
    _fields_ = [("length", W.WORD), ("maximum", W.WORD), ("buffer", W.LPWSTR)]


class ObjectAttributes(ctypes.Structure):
    _fields_ = [
        ("length", W.DWORD),
        ("parent", W.HANDLE),
        ("name", P),
        ("flags", W.DWORD),
        ("security", P),
        ("quality", P),
    ]


class IoStatus(ctypes.Structure):
    _fields_ = [("status", P), ("information", ctypes.c_size_t)]


def relative_open(parent, leaf, access, *, create=False, directory=None, security=None):
    """Return a caller-owned handle; creation is exclusive with the supplied SD."""
    length = len(leaf.encode("utf-16-le", "surrogatepass"))
    if (
        not length
        or length > 65532
        or leaf in (".", "..")
        or any(char in leaf for char in "\\/\0:")
        or leaf[-1] in " ."
    ):
        raise ServiceError("invalid_windows_state_path")
    text = ctypes.create_unicode_buffer(leaf, length // 2 + 1)
    name = UnicodeName(length, length + 2, ctypes.cast(text, W.LPWSTR))
    objects = ObjectAttributes(
        ctypes.sizeof(ObjectAttributes),
        parent,
        ctypes.addressof(name),
        0x1040,
        security,
        None,
    )  # OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE.
    handle, status = W.HANDLE(), IoStatus()
    options = 0x00200020  # OPEN_REPARSE_POINT | SYNCHRONOUS_IO_NONALERT.
    if directory is not None:
        options |= 1 if directory else 0x40
    result = nt_create(
        ctypes.byref(handle),
        access | 0x100000,
        ctypes.byref(objects),
        ctypes.byref(status),
        None,
        0x80,
        0 if create and not directory else 1,
        2 if create else 1,
        options,
        None,
        0,
    )
    if result < 0:
        raise ctypes.WinError(dos_error(result))
    return handle.value
