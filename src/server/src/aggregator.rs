use std::sync::Arc;

use crate::{orderbook_snapshot::OrderbookSnapshot, spmc::Spmc};
use keyrock_challenge_proto::orderbook::{Level, Summary};

use tokio::sync::Mutex;

const DEPTH: usize = 10;
const LEAD_TOLERANCE: usize = 3;

fn copy_level(level: &Level) -> Level {
    Level {
        price: level.price,
        amount: level.amount,
        exchange: (&level.exchange).to_string(),
    }
}

#[derive(Debug)]
pub struct Aggregator {
    best_bids_01: Option<[Level; DEPTH]>,
    best_bids_02: Option<[Level; DEPTH]>,
    best_asks_01: Option<[Level; DEPTH]>,
    best_asks_02: Option<[Level; DEPTH]>,
    spmc: Arc<Mutex<Spmc>>,
    exchange_01_name: String,
    exchange_02_name: String,
    lead_01: usize,
    lead_02: usize,
}

impl Aggregator {
    pub fn new(
        spmc: Arc<Mutex<Spmc>>,
        exchange_01_name: String,
        exchange_02_name: String,
    ) -> Aggregator {
        Aggregator {
            best_bids_01: None,
            best_bids_02: None,
            best_asks_01: None,
            best_asks_02: None,
            spmc,
            exchange_01_name,
            exchange_02_name,
            lead_01: 0,
            lead_02: 0,
        }
    }
    pub async fn process(&mut self, source_id: usize, snapshot: OrderbookSnapshot<DEPTH>) {
        match source_id {
            0 => {
                self.best_bids_01 = Some(snapshot.bids);
                self.best_asks_01 = Some(snapshot.asks);
                self.lead_01 += 1;
                self.lead_02 = 0;

                if Aggregator::stream_exceeded_lead_tolerance(self.lead_01) {
                    // In a production scenario, we might not even want to publish the aggregation here since it may not
                    // reflecting the actual spread anymore
                    Aggregator::log_lead_warning(&self.exchange_01_name, self.lead_01);
                }
            }
            1 => {
                self.best_bids_02 = Some(snapshot.bids);
                self.best_asks_02 = Some(snapshot.asks);
                self.lead_01 = 0;
                self.lead_02 = 1;

                if Aggregator::stream_exceeded_lead_tolerance(self.lead_01) {
                    // In a production scenario, we might not even want to publish the aggregation here since it may not
                    // reflecting the actual spread anymore
                    Aggregator::log_lead_warning(&self.exchange_02_name, self.lead_02);
                } 
            }
            _ => panic!("The aggregator currently only supports two market streams"),
        }

        if self.best_bids_01.is_some() && self.best_bids_02.is_some() {
            let mut merged_best_bids = Vec::<Level>::with_capacity(DEPTH);
            let mut merged_best_asks = Vec::<Level>::with_capacity(DEPTH);
            Aggregator::merge(
                &mut merged_best_bids,
                self.best_bids_01.as_ref().unwrap(),
                self.best_bids_02.as_ref().unwrap(),
                0,
                0,
                false,
            );
            Aggregator::merge(
                &mut merged_best_asks,
                self.best_asks_01.as_ref().unwrap(),
                self.best_asks_02.as_ref().unwrap(),
                0,
                0,
                true,
            );

            let mut smpc = self.spmc.lock().await;
            smpc.broadcast(Summary {
                spread: merged_best_asks.first().unwrap().price
                    - merged_best_bids.first().unwrap().price,
                bids: merged_best_bids,
                asks: merged_best_asks,
            })
            .await;
            return;
        }

        if self.best_bids_01.is_some() {
            let mut smpc = self.spmc.lock().await;
            smpc.broadcast(Summary {
                spread: self.best_asks_01.as_ref().unwrap().first().unwrap().price
                    - self.best_bids_01.as_ref().unwrap().first().unwrap().price,
                bids: self.best_bids_01.as_ref().unwrap().to_vec(),
                asks: self.best_asks_01.as_ref().unwrap().to_vec(),
            })
            .await
        } else {
            let mut smpc = self.spmc.lock().await;
            smpc.broadcast(Summary {
                spread: self.best_asks_02.as_ref().unwrap().first().unwrap().price
                    - self.best_bids_02.as_ref().unwrap().first().unwrap().price,
                bids: self.best_bids_02.as_ref().unwrap().to_vec(),
                asks: self.best_asks_02.as_ref().unwrap().to_vec(),
            })
            .await
        }
    }

    fn stream_exceeded_lead_tolerance(lead: usize) -> bool {
        lead >= LEAD_TOLERANCE
    }

    fn log_lead_warning(exchange_name: &str, lead: usize) {
        println!(
            "[WARNING]: {} stream is {} ticks ahead",
            exchange_name, lead
        );
    }

    /**
     * Merges two arrays of orderbook levels. 
     * Expects both arrays to be sorted with the best offer being at position 0.
     * The side states if the arrays contain bids (false) or asks (true)
     */
    fn merge(
        merged: &mut Vec<Level>,
        levels_01: &[Level; DEPTH],
        levels_02: &[Level; DEPTH],
        index_01: usize,
        index_02: usize,
        side: bool,
    ) {
        if merged.len() == merged.capacity() {
            return;
        }

        let mut new_index_01 = index_01;
        let mut new_index_02 = index_02;

        if side {
            // asks
            if new_index_01 >= DEPTH {
                merged.push(copy_level(&levels_02[index_02]));
                new_index_02 += 1;
            } else if new_index_02 >= DEPTH {
                merged.push(copy_level(&levels_01[index_01]));
                new_index_01 += 1;
            } else {
                let level_01 = &levels_01[index_01];
                let level_02 = &levels_02[index_02];

                if level_01.price > level_02.price {
                    merged.push(copy_level(level_02));
                    new_index_02 += 1;
                } else {
                    merged.push(copy_level(level_01));
                    new_index_01 += 1;
                }
            }
        } else {
            // bids
            if new_index_01 >= DEPTH {
                merged.push(copy_level(&levels_02[index_02]));
                new_index_02 += 1;
            } else if new_index_02 >= DEPTH {
                merged.push(copy_level(&levels_01[index_01]));
                new_index_01 += 1;
            } else {
                let level_01 = &levels_01[index_01];
                let level_02 = &levels_02[index_02];

                if level_01.price > level_02.price {
                    merged.push(copy_level(level_01));
                    new_index_01 += 1;
                } else {
                    merged.push(copy_level(level_02));
                    new_index_02 += 1;
                }
            }
        }

        Aggregator::merge(
            merged,
            levels_01,
            levels_02,
            new_index_01,
            new_index_02,
            side,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::Aggregator;
    use crate::aggregator::DEPTH;
    use init_with::InitWith;
    use keyrock_challenge_proto::orderbook::Level;

    #[test]
    fn should_merge_bids() {
        // Arrange
        let mut merged = Vec::<Level>::with_capacity(DEPTH);
        let levels_01 = <[Level; DEPTH]>::init_with_indices(|i| Level {
            price: 20. - i as f64,
            amount: 13.,
            exchange: String::new(),
        });
        let levels_02 = <[Level; DEPTH]>::init_with_indices(|i| Level {
            price: 26. - 2. * i as f64,
            amount: 37.,
            exchange: String::new(),
        });

        // Act
        Aggregator::merge(&mut merged, &levels_01, &levels_02, 0, 0, false);

        // Assert
        assert!(merged[0].amount == 37. && merged[0].price == 26.);
        assert!(merged[1].amount == 37. && merged[1].price == 24.);
        assert!(merged[2].amount == 37. && merged[2].price == 22.);
        assert!(merged[3].amount == 37. && merged[3].price == 20.);
        assert!(merged[4].amount == 13. && merged[4].price == 20.);
        assert!(merged[5].amount == 13. && merged[5].price == 19.);
        assert!(merged[6].amount == 37. && merged[6].price == 18.);
        assert!(merged[7].amount == 13. && merged[7].price == 18.);
        assert!(merged[8].amount == 13. && merged[8].price == 17.);
        assert!(merged[9].amount == 37. && merged[9].price == 16.);
    }

    #[test]
    fn should_merge_asks() {
        // Arrange
        let mut merged = Vec::<Level>::with_capacity(DEPTH);
        let levels_01 = <[Level; DEPTH]>::init_with_indices(|i| Level {
            price: 10. + i as f64,
            amount: 13.,
            exchange: String::new(),
        });
        let levels_02 = <[Level; DEPTH]>::init_with_indices(|i| Level {
            price: 6. + 2. * i as f64,
            amount: 37.,
            exchange: String::new(),
        });

        // Act
        Aggregator::merge(&mut merged, &levels_01, &levels_02, 0, 0, true);

        // Assert
        assert!(merged[0].amount == 37. && merged[0].price == 6.);
        assert!(merged[1].amount == 37. && merged[1].price == 8.);
        assert!(merged[2].amount == 13. && merged[2].price == 10.);
        assert!(merged[3].amount == 37. && merged[3].price == 10.);
        assert!(merged[4].amount == 13. && merged[4].price == 11.);
        assert!(merged[5].amount == 13. && merged[5].price == 12.);
        assert!(merged[6].amount == 37. && merged[6].price == 12.);
        assert!(merged[7].amount == 13. && merged[7].price == 13.);
        assert!(merged[8].amount == 13. && merged[8].price == 14.);
        assert!(merged[9].amount == 37. && merged[9].price == 14.);
    }

    #[test]
    fn should_merge_real_data_bids() {
        // Arrange
        let mut merged = Vec::<Level>::with_capacity(DEPTH * 2);
        let levels_01 = [
            Level {
                price: 0.074505000000000002,
                amount: 1.,
                exchange: "Binance".to_string(),
            },
            Level {
                price: 0.074501999999999999,
                amount: 1.,
                exchange: "Binance".to_string(),
            },
            Level {
                price: 0.074500999999999998,
                amount: 1.,
                exchange: "Binance".to_string(),
            },
            Level {
                price: 0.074496000000000007,
                amount: 1.,
                exchange: "Binance".to_string(),
            },
            Level {
                price: 0.074492000000000003,
                amount: 1.,
                exchange: "Binance".to_string(),
            },
            Level {
                price: 0.074490000000000001,
                amount: 1.,
                exchange: "Binance".to_string(),
            },
            Level {
                price: 0.074489,
                amount: 1.,
                exchange: "Binance".to_string(),
            },
            Level {
                price: 0.074487999999999999,
                amount: 1.,
                exchange: "Binance".to_string(),
            },
            Level {
                price: 0.074485999999999997,
                amount: 1.,
                exchange: "Binance".to_string(),
            },
            Level {
                price: 0.074484999999999996,
                amount: 1.,
                exchange: "Binance".to_string(),
            },
        ];
        let levels_02 = [
            Level {
                price: 0.074488570000000004,
                amount: 1.,
                exchange: "Bitstamp".to_string(),
            },
            Level {
                price: 0.074467909999999998,
                amount: 1.,
                exchange: "Bitstamp".to_string(),
            },
            Level {
                price: 0.074462249999999994,
                amount: 1.,
                exchange: "Bitstamp".to_string(),
            },
            Level {
                price: 0.074442809999999998,
                amount: 1.,
                exchange: "Bitstamp".to_string(),
            },
            Level {
                price: 0.074435570000000006,
                amount: 1.,
                exchange: "Bitstamp".to_string(),
            },
            Level {
                price: 0.074430650000000001,
                amount: 1.,
                exchange: "Bitstamp".to_string(),
            },
            Level {
                price: 0.074423119999999995,
                amount: 1.,
                exchange: "Bitstamp".to_string(),
            },
            Level {
                price: 0.074420920000000002,
                amount: 1.,
                exchange: "Bitstamp".to_string(),
            },
            Level {
                price: 0.074418860000000003,
                amount: 1.,
                exchange: "Bitstamp".to_string(),
            },
            Level {
                price: 0.074410000000000004,
                amount: 1.,
                exchange: "Bitstamp".to_string(),
            },
        ];

        // Act
        Aggregator::merge(&mut merged, &levels_01, &levels_02, 0, 0, false);

        // Assert
        assert!(
            merged[0].price == 0.074505000000000002 && merged[0].exchange == "Binance".to_string()
        );
        assert!(
            merged[1].price == 0.074501999999999999 && merged[1].exchange == "Binance".to_string()
        );
        assert!(
            merged[2].price == 0.074500999999999998 && merged[2].exchange == "Binance".to_string()
        );
        assert!(
            merged[3].price == 0.074496000000000007 && merged[3].exchange == "Binance".to_string()
        );
        assert!(
            merged[4].price == 0.074492000000000003 && merged[4].exchange == "Binance".to_string()
        );
        assert!(
            merged[5].price == 0.074490000000000001 && merged[5].exchange == "Binance".to_string()
        );
        assert!(merged[6].price == 0.074489 && merged[6].exchange == "Binance".to_string());
        assert!(
            merged[7].price == 0.074488570000000004 && merged[7].exchange == "Bitstamp".to_string()
        );
        assert!(
            merged[8].price == 0.074487999999999999 && merged[8].exchange == "Binance".to_string()
        );
        assert!(
            merged[9].price == 0.074485999999999997 && merged[9].exchange == "Binance".to_string()
        );
        assert!(
            merged[10].price == 0.074484999999999996
                && merged[10].exchange == "Binance".to_string()
        );
        assert!(
            merged[11].price == 0.074467909999999998
                && merged[11].exchange == "Bitstamp".to_string()
        );
        assert!(
            merged[12].price == 0.074462249999999994
                && merged[12].exchange == "Bitstamp".to_string()
        );
        assert!(
            merged[19].price == 0.074410000000000004
                && merged[19].exchange == "Bitstamp".to_string()
        );
    }
}
