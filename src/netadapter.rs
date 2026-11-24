//! Which `GetIfTable2` rows are real interfaces.
//!
//! NDIS lightweight filters (WFP, QoS, virtual-switch extensions) sit on the
//! same adapter and repeat its octets. Summing them counts the same traffic
//! twice. Loopback is not a NIC. A down adapter is hidden when something else
//! is up, so disconnected `ローカル エリア接続*` rows do not fill the list.
//! An up adapter with a measured rate of 0 stays in the list as `0 B/s`.

/// One row before the keep/drop decision. Octets are cumulative counters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterCandidate {
    pub index: u32,
    pub name: String,
    /// `InOctets`. This is receive, not send.
    pub in_octets: u64,
    /// `OutOctets`. This is send, not receive.
    pub out_octets: u64,
    pub filter: bool,
    pub hardware: bool,
    pub endpoint: bool,
    pub loopback: bool,
    pub oper_up: bool,
}

/// A row that is allowed into the total and the per-adapter list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CountedAdapter {
    pub index: u32,
    pub name: String,
    /// Copied from [`AdapterCandidate::in_octets`].
    pub recv_octets: u64,
    /// Copied from [`AdapterCandidate::out_octets`].
    pub send_octets: u64,
}

/// Bits of `MIB_IF_ROW2.InterfaceAndOperStatusFlags`.
///
/// `netioapi.h` declares the 1-bit fields in this order, and MSVC allocates
/// them from the low bit of the flag byte:
/// hardware, filter, connector present, not authenticated, not media
/// connected, paused, low power, endpoint.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IfRowFlags {
    pub hardware: bool,
    pub filter: bool,
    pub endpoint: bool,
}

#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub fn if_row_flags(bits: u8) -> IfRowFlags {
    IfRowFlags {
        hardware: bits & 0b0000_0001 != 0,
        filter: bits & 0b0000_0010 != 0,
        endpoint: bits & 0b1000_0000 != 0,
    }
}

/// Drop filter and loopback rows. Prefer oper-up interfaces.
///
/// Hardware and endpoint flags are recorded so a virtual NIC (Tailscale, WSL,
/// Hyper-V) is kept when it is up and not a filter. They are not required:
/// an up non-filter row is a real interface even if both flags are clear.
/// When every remaining row is down, those rows are returned so the page is
/// not an empty `N/A`. Zero cumulative octets are still returned.
pub fn select_adapters(rows: &[AdapterCandidate]) -> Vec<CountedAdapter> {
    let mut real: Vec<&AdapterCandidate> = rows
        .iter()
        .filter(|row| !row.filter && !row.loopback)
        .collect();
    let any_up = real.iter().any(|row| row.oper_up);
    if any_up {
        real.retain(|row| row.oper_up);
    }
    real.sort_by(|left, right| {
        right
            .hardware
            .cmp(&left.hardware)
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.index.cmp(&right.index))
    });
    real.into_iter()
        .map(|row| CountedAdapter {
            index: row.index,
            name: row.name.clone(),
            recv_octets: row.in_octets,
            send_octets: row.out_octets,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::too_many_arguments)]
    fn row(
        index: u32,
        name: &str,
        in_octets: u64,
        out_octets: u64,
        filter: bool,
        hardware: bool,
        endpoint: bool,
        loopback: bool,
        oper_up: bool,
    ) -> AdapterCandidate {
        AdapterCandidate {
            index,
            name: name.to_string(),
            in_octets,
            out_octets,
            filter,
            hardware,
            endpoint,
            loopback,
            oper_up,
        }
    }

    #[test]
    fn filter_bindings_are_not_added_to_the_base_adapter() {
        let rows = vec![
            row(
                1,
                "イーサネット",
                1_000,
                2_000,
                false,
                true,
                true,
                false,
                true,
            ),
            row(
                2,
                "イーサネット-WFP Native MAC Layer LightWeight Filter-0000",
                1_000,
                2_000,
                true,
                false,
                false,
                false,
                true,
            ),
            row(
                3,
                "イーサネット-QoS Packet Scheduler-0000",
                1_000,
                2_000,
                true,
                false,
                false,
                false,
                true,
            ),
            row(
                4,
                "イーサネット-WFP 802.3 MAC Layer LightWeight Filter-0000",
                1_000,
                2_000,
                true,
                false,
                false,
                false,
                true,
            ),
            row(
                5,
                "vSwitch (WSL)-Virtual Switch Extension Filter-0000",
                50,
                50,
                true,
                false,
                false,
                false,
                true,
            ),
            row(
                6,
                "vEthernet (WSL (Hyper-V firewall))",
                0,
                0,
                false,
                false,
                true,
                false,
                true,
            ),
            row(7, "Tailscale", 10, 400, false, false, true, false, true),
            row(
                8,
                "ローカル エリア接続* 6",
                80,
                80,
                false,
                false,
                true,
                false,
                false,
            ),
            row(
                9,
                "Loopback Pseudo-Interface 1",
                9_000,
                9_000,
                false,
                false,
                false,
                true,
                true,
            ),
        ];
        let kept = select_adapters(&rows);
        let names: Vec<&str> = kept.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "イーサネット",
                "Tailscale",
                "vEthernet (WSL (Hyper-V firewall))",
            ]
        );
        assert_eq!(kept[0].recv_octets, 1_000);
        assert_eq!(kept[0].send_octets, 2_000);
        let recv: u64 = kept.iter().map(|row| row.recv_octets).sum();
        let send: u64 = kept.iter().map(|row| row.send_octets).sum();
        assert_eq!(recv, 1_010);
        assert_eq!(send, 2_400);
        assert!(kept
            .iter()
            .any(|row| row.recv_octets == 0 && row.send_octets == 0));
    }

    #[test]
    fn down_adapters_remain_when_nothing_is_up() {
        let rows = vec![
            row(1, "イーサネット", 5, 6, false, true, true, false, false),
            row(
                2,
                "イーサネット-QoS Packet Scheduler-0000",
                5,
                6,
                true,
                false,
                false,
                false,
                false,
            ),
        ];
        let kept = select_adapters(&rows);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].name, "イーサネット");
        assert_eq!(kept[0].recv_octets, 5);
        assert_eq!(kept[0].send_octets, 6);
    }

    #[test]
    fn flag_bits_follow_the_header_order() {
        let flags = if_row_flags(0b1000_0011);
        assert!(flags.hardware);
        assert!(flags.filter);
        assert!(flags.endpoint);
        let only_endpoint = if_row_flags(0b1000_0000);
        assert!(!only_endpoint.hardware);
        assert!(!only_endpoint.filter);
        assert!(only_endpoint.endpoint);
    }
}
