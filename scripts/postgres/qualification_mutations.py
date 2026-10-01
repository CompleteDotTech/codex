"""Reversible SQL mutations on caller-owned, isolated qualification fixtures."""

from contextlib import contextmanager

from qualification_checks import sql


@contextmanager
def temporary_sql(home, setup, cleanup):
    """Arm atomic cleanup before setup, whose commit result may be lost."""
    failure = None
    try:
        sql(home, f"BEGIN; {setup}; COMMIT")
        yield
    except BaseException as error:
        failure = error
        raise
    finally:
        try:
            sql(home, f"BEGIN; {cleanup}; COMMIT")
        except BaseException:
            if failure is None:
                raise
