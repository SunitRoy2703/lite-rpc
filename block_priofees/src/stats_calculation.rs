use crate::rpc_data::PrioFeesStats;
use itertools::Itertools;
use log::info;
use std::collections::HashMap;

pub fn calculate_supp_stats(
    // Vec(prioritization_fees, cu_consumed)
    prio_fees_in_block: &Vec<(u64, u64)>,
) -> PrioFeesStats {
    let mut prio_fees_in_block = if prio_fees_in_block.is_empty() {
        // TODO is that smart?
        vec![(0, 0)]
    } else {
        prio_fees_in_block.clone()
    };
    // sort by prioritization fees
    prio_fees_in_block.sort_by(|a, b| a.0.cmp(&b.0));

    // get stats by transaction
    let median_index = prio_fees_in_block.len() / 2;
    let p75_index = prio_fees_in_block.len() * 75 / 100;
    let p90_index = prio_fees_in_block.len() * 90 / 100;
    let p_min = prio_fees_in_block[0].0;
    let p_median = prio_fees_in_block[median_index].0;
    let p_75 = prio_fees_in_block[p75_index].0;
    let p_90 = prio_fees_in_block[p90_index].0;
    let p_max = prio_fees_in_block.last().map(|x| x.0).unwrap();

    let dist_fee_by_index: Vec<(String, u64)> =
        (0..=100).step_by(5)
        .map(|p| {
            let prio_fee = if p == 100 {
                prio_fees_in_block.last().unwrap().0
            } else {
                let index = prio_fees_in_block.len() * p / 100;
                prio_fees_in_block[index].0
            };
            (format!("p{}", p), prio_fee)
        })
        .collect_vec();

    // assert_eq!(p_min, *fine_percentiles.get("p0").unwrap());
    // assert_eq!(p_median, *fine_percentiles.get("p50").unwrap());
    // assert_eq!(p_75, *fine_percentiles.get("p75").unwrap());
    // assert_eq!(p_90, *fine_percentiles.get("p90").unwrap());
    // assert_eq!(p_max, *fine_percentiles.get("p100").unwrap());

    // get stats by CU
    // e.g. 95 -> 3000
    let mut dist_fee_by_cu: HashMap<i32, u64> = HashMap::new();
    let mut med_cu = None;
    let mut p75_cu = None;
    let mut p90_cu = None;
    let mut p95_cu = None;
    let cu_sum: u64 = prio_fees_in_block.iter().map(|x| x.1).sum();
    let mut agg: u64 = 0;
    for (prio, cu) in prio_fees_in_block {
        agg = agg + cu;

        if med_cu.is_none() && agg > (cu_sum as f64 * 0.5) as u64 {
            med_cu = Some(prio);
        }
        if p75_cu.is_none() && agg > (cu_sum as f64 * 0.75) as u64 {
            p75_cu = Some(prio)
        }
        if p90_cu.is_none() && agg > (cu_sum as f64 * 0.9) as u64 {
            p90_cu = Some(prio);
        }
        if p95_cu.is_none() && agg > (cu_sum as f64 * 0.95) as u64 {
            p95_cu = Some(prio);
        }

        for p in (0..=100).step_by(5) {
            if !dist_fee_by_cu.contains_key(&p) {
                if agg > (cu_sum as f64 * p as f64 / 100.0) as u64 {
                    dist_fee_by_cu.insert(p, prio);
                }
            }
        }
    }

    // assert_eq!(med_cu.as_ref(), fine_percentiles_cu.get(&50));
    // assert_eq!(p75_cu.as_ref(), fine_percentiles_cu.get(&75));
    // assert_eq!(p90_cu.as_ref(), fine_percentiles_cu.get(&90));
    // assert_eq!(p95_cu.as_ref(), fine_percentiles_cu.get(&95));

    // e.g. (p0, 0), (p5, 100), (p10, 200), ..., (p95, 3000), (p100, 3000)
    let dist_fee_by_cu: Vec<(String, u64)> =
        dist_fee_by_cu
        .into_iter()
        .sorted_by_key(|(p, _)| *p)
        .map(|(p, fees)| (format!("p{}", p), fees))
        .collect_vec();

    PrioFeesStats {
        p_min,
        p_median,
        p_75,
        p_90,
        p_max,
        dist_fee_by_index,
        p_median_cu: med_cu.unwrap_or(0),
        p_75_cu: p75_cu.unwrap_or(0),
        p_90_cu: p90_cu.unwrap_or(0),
        p_95_cu: p95_cu.unwrap_or(0),
        dist_fee_by_cu,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_supp_info() {
        let prio_fees_in_block = vec![(2, 2), (4, 4), (5, 5), (3, 3), (1, 1)];
        let supp_info = calculate_supp_stats(&prio_fees_in_block);
        assert_eq!(supp_info.p_min, 1);
        assert_eq!(supp_info.p_median, 3);
        assert_eq!(supp_info.p_75, 4);
        assert_eq!(supp_info.p_90, 5);
        assert_eq!(supp_info.p_max, 5);
    }

    #[test]
    fn test_statisticshowto() {
        let prio_fees_in_block = vec![
            (30, 1),
            (33, 2),
            (43, 3),
            (53, 4),
            (56, 5),
            (67, 6),
            (68, 7),
            (72, 8),
        ];
        let supp_info = calculate_supp_stats(&prio_fees_in_block);
        println!("supp_info.dist_fee {:?}", &supp_info.dist_fee_by_index);
        assert_eq!(supp_info.dist_fee_by_index[5], ("p25".to_string(), 43));
    }

    #[test]
    fn test_large_list() {
        let prio_fees_in_block: Vec<(u64, u64)> = (0..1000).map(|x| (x, x)).collect();
        let supp_info = calculate_supp_stats(&prio_fees_in_block);
        assert_eq!(supp_info.dist_fee_by_index[19], ("p95".to_string(), 950));
    }
}
