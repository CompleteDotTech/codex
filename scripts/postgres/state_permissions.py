"""Private state creation and fail-closed platform permission validation."""

import os

from state_io import ServiceError


def private_directory(path):
    if os.name == "nt":
        from windows_state import create_directory

        create_directory(path)
    else:
        from posix_io import create_directory

        create_directory(path)


def _validate_windows_permissions(paths):
    """Compatibility inspection; trusted reads must use pinned_paths themselves."""
    from windows_state import pinned_paths

    try:
        with pinned_paths() as scope:
            for path in paths:
                scope.validate(path)
    except (OSError, ServiceError):
        raise ServiceError(
            "insecure_or_unverifiable_windows_state_permissions"
        ) from None
