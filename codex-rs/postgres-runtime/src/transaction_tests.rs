use super::TransactionError;
use std::io;

#[test]
fn missing_commit_response_is_not_treated_as_an_abort() {
    let lost_response = sqlx::Error::Io(io::Error::new(io::ErrorKind::ConnectionReset, "closed"));
    assert_eq!(
        TransactionError::classify_commit(&lost_response),
        TransactionError::CommitOutcomeUnknown
    );
    assert_eq!(
        TransactionError::classify_statement(&lost_response),
        TransactionError::Unavailable
    );
}

#[test]
fn commit_sqlstates_distinguish_abort_from_lost_response() {
    for code in [
        None,
        Some("08006"),
        Some("57P01"),
        Some("57P02"),
        Some("57P03"),
    ] {
        assert_eq!(
            TransactionError::classify_commit_sqlstate(code),
            TransactionError::CommitOutcomeUnknown
        );
    }
    assert_eq!(
        TransactionError::classify_commit_sqlstate(Some("40001")),
        TransactionError::SerializationConflict
    );
    assert_eq!(
        TransactionError::classify_commit_sqlstate(Some("40P01")),
        TransactionError::Deadlock
    );
    assert_eq!(
        TransactionError::classify_commit_sqlstate(Some("23505")),
        TransactionError::Rejected
    );
}
