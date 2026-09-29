use super::BootstrapError;
use super::classify_migration;
use super::classify_namespace_validation;
use super::classify_sqlx;
use pretty_assertions::assert_eq;
use sqlx::migrate::MigrateError;

#[test]
fn classify_post_acquisition_transport_failures_as_unavailable() {
    for error in [
        sqlx::Error::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "connection reset",
        )),
        sqlx::Error::Protocol("connection protocol failed".to_string()),
        sqlx::Error::PoolTimedOut,
        sqlx::Error::PoolClosed,
        sqlx::Error::WorkerCrashed,
        sqlx::Error::BeginFailed,
    ] {
        assert_eq!(classify_sqlx(&error), BootstrapError::Unavailable);
    }
    assert_eq!(
        classify_migration(MigrateError::Execute(sqlx::Error::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "connection reset"
        ),))),
        BootstrapError::Unavailable
    );
    assert_eq!(
        classify_sqlx(&sqlx::Error::RowNotFound),
        BootstrapError::Migration
    );
    assert_eq!(
        classify_namespace_validation(sqlx::Error::ColumnDecode {
            index: "format_version".to_string(),
            source: Box::new(sqlx::error::UnexpectedNullError),
        }),
        BootstrapError::IncompatibleNamespace
    );
}
