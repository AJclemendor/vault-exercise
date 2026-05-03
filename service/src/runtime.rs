use std::env;
use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub(crate) struct BalanceTuning {
    pub(crate) admission_cache_max_age: Duration,
    pub(crate) active_cache_max_age: Duration,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TaskTuning {
    pub(crate) active_refresh_interval: Duration,
    pub(crate) log_poll_interval: Duration,
    pub(crate) stats_log_interval: Duration,
    pub(crate) active_refresh_budget: usize,
    pub(crate) log_reorg_depth: u64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ChainTuning {
    pub(crate) rpc_http_timeout: Duration,
    pub(crate) receipt_confirm_retries: usize,
    pub(crate) receipt_confirm_retry_sleep: Duration,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ReceiptTuning {
    pub(crate) uncertain_rechecks: usize,
    pub(crate) uncertain_recheck_interval: Duration,
    pub(crate) deferred_rechecks: usize,
    pub(crate) deferred_recheck_interval: Duration,
}

pub(crate) fn balance_tuning() -> BalanceTuning {
    BalanceTuning {
        admission_cache_max_age: duration_ms_env("ADMISSION_CACHE_MAX_AGE_MS", 3_000),
        active_cache_max_age: duration_ms_env("ACTIVE_CACHE_MAX_AGE_MS", 900),
    }
}

pub(crate) fn task_tuning() -> TaskTuning {
    TaskTuning {
        active_refresh_interval: duration_ms_env("ACTIVE_REFRESH_INTERVAL_MS", 300),
        log_poll_interval: duration_ms_env("LOG_POLL_INTERVAL_MS", 250),
        stats_log_interval: duration_ms_env("STATS_LOG_INTERVAL_MS", 5_000),
        active_refresh_budget: usize_env("ACTIVE_REFRESH_BUDGET", 40).max(1),
        log_reorg_depth: u64_env("LOG_REORG_DEPTH", 6),
    }
}

pub(crate) fn chain_tuning() -> ChainTuning {
    ChainTuning {
        rpc_http_timeout: duration_ms_env("RPC_HTTP_TIMEOUT_MS", 5_000),
        receipt_confirm_retries: usize_env("RECEIPT_CONFIRM_RETRIES", 3).max(1),
        receipt_confirm_retry_sleep: duration_ms_env("RECEIPT_CONFIRM_RETRY_SLEEP_MS", 250),
    }
}

pub(crate) fn receipt_tuning() -> ReceiptTuning {
    ReceiptTuning {
        uncertain_rechecks: usize_env("UNCERTAIN_RECEIPT_RECHECKS", 20).max(1),
        uncertain_recheck_interval: duration_ms_env("UNCERTAIN_RECEIPT_RECHECK_INTERVAL_MS", 250),
        deferred_rechecks: usize_env("DEFERRED_RECEIPT_RECHECKS", 30).max(1),
        deferred_recheck_interval: duration_ms_env("DEFERRED_RECEIPT_RECHECK_INTERVAL_MS", 1_000),
    }
}

fn duration_ms_env(name: &str, default_ms: u64) -> Duration {
    Duration::from_millis(u64_env(name, default_ms))
}

fn usize_env(name: &str, default: usize) -> usize {
    match env::var(name) {
        Ok(value) => value.parse().unwrap_or_else(|_| {
            eprintln!("[config] invalid {name}={value:?}; using default {default}");
            default
        }),
        Err(_) => default,
    }
}

fn u64_env(name: &str, default: u64) -> u64 {
    match env::var(name) {
        Ok(value) => value.parse().unwrap_or_else(|_| {
            eprintln!("[config] invalid {name}={value:?}; using default {default}");
            default
        }),
        Err(_) => default,
    }
}
