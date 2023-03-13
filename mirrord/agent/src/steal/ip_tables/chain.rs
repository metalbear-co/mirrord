use std::sync::atomic::{AtomicI32, Ordering};

use crate::{error::Result, steal::ip_tables::IPTables};

#[derive(Debug)]
pub struct IPTableChain<'ipt, IPT> {
    inner: &'ipt IPT,
    chain: &'ipt str,
    chain_size: AtomicI32,
}

impl<'ipt, IPT> IPTableChain<'ipt, IPT>
where
    IPT: IPTables,
{
    pub fn create(inner: &'ipt IPT, chain: &'ipt str) -> Result<Self> {
        inner.create_chain(chain)?;

        // Start with 1 because the chain will allways have atleast `-A <chain name>` as a rule
        let chain_size = AtomicI32::from(1);

        Ok(IPTableChain {
            inner,
            chain,
            chain_size,
        })
    }

    pub fn add_rule(&self, rule: &str) -> Result<i32> {
        self.inner
            .insert_rule(
                &self.chain,
                rule,
                self.chain_size.fetch_add(1, Ordering::Relaxed),
            )
            .map(|_| self.chain_size.load(Ordering::Relaxed))
            .map_err(|err| {
                self.chain_size.fetch_sub(1, Ordering::Relaxed);
                err
            })
    }

    pub fn remove_rule(&self, rule: &str) -> Result<()> {
        self.inner.remove_rule(&self.chain, rule)?;

        self.chain_size.fetch_sub(1, Ordering::Relaxed);

        Ok(())
    }
}
