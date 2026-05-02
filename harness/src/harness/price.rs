use rand::SeedableRng;
use rand::rngs::SmallRng;
use tokio::time::{self, Duration};
use tokio_util::sync::CancellationToken;

use super::generators::FairPrice;

pub fn spawn(fair: FairPrice, cancel: CancellationToken) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut rng = SmallRng::from_os_rng();
        let mut interval = time::interval(Duration::from_millis(100));

        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = interval.tick() => fair.drift(&mut rng),
            }
        }
    })
}
