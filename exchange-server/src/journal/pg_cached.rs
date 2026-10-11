//! Bounded, connection-local prepared statements. Transaction boundaries are
//! unchanged; dropping a transaction still delegates rollback to postgres.
use postgres::{Client, Error, NoTls, Row, Statement, Transaction, types::ToSql};
use std::{
    collections::BTreeMap,
    ops::{Deref, DerefMut},
};

const STATEMENT_LIMIT: usize = 128;
type Statements = BTreeMap<String, Statement>;

pub(crate) struct CachedClient {
    inner: Client,
    statements: Statements,
}
pub(super) struct CachedTransaction<'a> {
    inner: Transaction<'a>,
    statements: &'a mut Statements,
}

macro_rules! query_methods {
    () => {
        fn statement(&mut self, sql: &str) -> Result<Statement, Error> {
            if let Some(statement) = self.statements.get(sql) {
                return Ok(statement.clone());
            }
            let statement = self.inner.prepare(sql)?;
            if self.statements.len() >= STATEMENT_LIMIT {
                self.statements.clear();
            }
            self.statements.insert(sql.to_owned(), statement.clone());
            Ok(statement)
        }
        pub(super) fn execute(
            &mut self,
            sql: &str,
            params: &[&(dyn ToSql + Sync)],
        ) -> Result<u64, Error> {
            let statement = self.statement(sql)?;
            self.inner.execute(&statement, params)
        }
        pub(super) fn query(
            &mut self,
            sql: &str,
            params: &[&(dyn ToSql + Sync)],
        ) -> Result<Vec<Row>, Error> {
            let statement = self.statement(sql)?;
            self.inner.query(&statement, params)
        }
        pub(super) fn query_one(
            &mut self,
            sql: &str,
            params: &[&(dyn ToSql + Sync)],
        ) -> Result<Row, Error> {
            let statement = self.statement(sql)?;
            self.inner.query_one(&statement, params)
        }
        pub(super) fn query_opt(
            &mut self,
            sql: &str,
            params: &[&(dyn ToSql + Sync)],
        ) -> Result<Option<Row>, Error> {
            let statement = self.statement(sql)?;
            self.inner.query_opt(&statement, params)
        }
    };
}
impl CachedClient {
    pub(super) fn connect(url: &str, tls: NoTls) -> Result<Self, Error> {
        Ok(Self {
            inner: Client::connect(url, tls)?,
            statements: Statements::new(),
        })
    }
    pub(super) fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }
    pub(super) fn transaction(&mut self) -> Result<CachedTransaction<'_>, Error> {
        Ok(CachedTransaction {
            inner: self.inner.transaction()?,
            statements: &mut self.statements,
        })
    }
    query_methods!();
}
impl CachedTransaction<'_> {
    pub(super) fn commit(self) -> Result<(), Error> {
        self.inner.commit()
    }
    query_methods!();
}
impl Deref for CachedClient {
    type Target = Client;
    fn deref(&self) -> &Client {
        &self.inner
    }
}
impl DerefMut for CachedClient {
    fn deref_mut(&mut self) -> &mut Client {
        &mut self.inner
    }
}
impl<'a> Deref for CachedTransaction<'a> {
    type Target = Transaction<'a>;
    fn deref(&self) -> &Transaction<'a> {
        &self.inner
    }
}
impl DerefMut for CachedTransaction<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}
