use bitflags::bitflags;
use std::collections::{BTreeMap, VecDeque};
use std::io::prelude::*;
use std::{io, time};

bitflags! {
    pub(crate) struct Available: u8 {
        const READ = 0b00000001;
        const WRITE = 0b00000010;
    }
}

#[derive(Debug)]
enum State {
    Listen,
    SynSent,
    SynRcvd,
    Estab,
    FinWait1,
    FinWait2,
    CloseWait,
    LastAck,
    Closing,
    TimeWait,
    Closed,
}

// impl State {
//     fn is_synchronized(&self) -> bool {
//         match *self {
//             State::SynRcvd => false,
//             State::Estab | State::FinWait1 | State::FinWait2 | State::TimeWait => true,
//         }
//     }
// }

pub struct Connection {
    state: State,
    send: SendSequenceSpace,
    recv: RecvSequenceSpace,
    pub ip: etherparse::Ipv4Header,
    tcp: etherparse::TcpHeader,
    timers: Timers,

    pub(crate) incoming: VecDeque<u8>, // pub(crate) == protected keyword
    pub(crate) unacked: VecDeque<u8>,  // unacked contains both sent and unsent data

    pub(crate) closed: bool,
    closed_at: Option<u32>,
}

struct Timers {
    send_times: BTreeMap<u32, time::Instant>,
    srtt: f64,
}

impl Connection {
    pub(crate) fn is_rcv_closed(&self) -> bool {
        if let State::TimeWait
        | State::CloseWait
        | State::LastAck
        | State::Closed
        | State::Closing = self.state
        {
            true
        } else {
            false
        }
    }

    pub(crate) fn is_snd_closed(&self) -> bool {
        if let State::FinWait1
        | State::FinWait2
        | State::Closing
        | State::TimeWait
        | State::LastAck
        | State::Closed = self.state
        {
            true
        } else {
            false
        }
    }

    fn availability(&self) -> Available {
        let mut a = Available::empty();

        if self.is_rcv_closed() || !self.incoming.is_empty() {
            a |= Available::READ;
        }

        if self.is_snd_closed() || !self.unacked.is_empty() {
            a |= Available::WRITE;
        }

        // done -> test it
        // // TODO: set Available::write
        // // TODO: take into account self.state
        a
    }
}

/// State of the Send Sequence Space (RFC 793 S3.2 Figure4)
///
/// ```
///
///      1         2          3          4
/// ----------|----------|----------|----------
///        SND.UNA    SND.NXT    SND.UNA    
///                             + SND.WND
///
/// 1 - old sequence numbers which have been acknowledged
/// 2 - sequence numbers of unacknowledged data
/// 3 - sequence numbers allowed for new data transmission
/// 4 - future sequence numbers which are not yet allowed
/// ```

pub struct SendSequenceSpace {
    // send unacknowledged
    una: u32,
    // send next
    nxt: u32,
    // send window
    wnd: u16,
    // send urgent pointer
    up: bool,
    // segment sequence number used for last window update
    wl1: usize,
    // segment acknowledgment number used for last window update
    wl2: usize,
    // initial send sequence number
    iss: u32,
}

/// State of the Receive Sequence Space (RFC 793 S3.2 Figure5)
///
/// ```

///      1          2          3
/// ----------|----------|----------
///        RCV.NXT    RCV.NXT
///                  + RCV.WND
///
/// 1 - old sequence numbers which have been acknowledged
/// 2 - sequence numbers allowed for new reception
/// 3 - future sequence numbers which are not yet allowed
/// ```

pub struct RecvSequenceSpace {
    // receive next
    nxt: u32,
    // receive window
    wnd: u16,
    //  receive urgent pointer
    up: bool,
    // initial receive sequence number
    irs: u32,
}

impl Connection {
    pub fn accept<'a>(
        nic: &mut tun_tap::Iface,
        ip_h: etherparse::Ipv4HeaderSlice<'a>,
        tcp_h: etherparse::TcpHeaderSlice<'a>,
        data: &'a [u8],
    ) -> io::Result<Option<Self>> {
        let buf = [0u8; 1500];
        if !tcp_h.syn() {
            // only expected SYN packet
            return Ok(None);
        }

        let iss = 0;
        let wnd = 1024;
        let mut c = Connection {
            state: State::SynRcvd,
            send: SendSequenceSpace {
                iss: iss,
                una: iss,
                nxt: iss,
                wnd: wnd,
                up: false,

                wl1: 0,
                wl2: 0,
            },
            recv: RecvSequenceSpace {
                irs: tcp_h.sequence_number(),
                nxt: tcp_h.sequence_number() + 1,
                wnd: tcp_h.window_size(),
                up: false,
            },
            ip: etherparse::Ipv4Header::new(
                0,
                64,
                etherparse::IpTrafficClass::Tcp,
                ip_h.destination().try_into().unwrap(),
                ip_h.source().try_into().unwrap(),
            ),
            tcp: etherparse::TcpHeader::new(
                tcp_h.destination_port(),
                tcp_h.source_port(),
                iss,
                wnd,
            ),

            incoming: Default::default(),
            unacked: Default::default(),

            closed: false,
            closed_at: None,

            timers: Timers {
                send_times: Default::default(),
                srtt: time::Duration::from_secs(1 * 60).as_secs_f64(),
            },
        };

        // needs to start establishing connection
        c.tcp.syn = true;
        c.tcp.ack = true;
        c.write(nic, c.send.nxt, 0)?;
        Ok(Some(c))
    }

    pub fn connect<'a>(
        nic: &mut tun_tap::Iface,
        ip_h: etherparse::Ipv4HeaderSlice<'a>,
        tcp_h: etherparse::TcpHeaderSlice<'a>,
        data: &'a [u8],
    ) -> io::Result<Option<Self>> {
        let buf = [0u8; 1500];
        if !tcp_h.syn() {
            // expected SYN packet
            return Ok(None);
        }

        let iss = 0;
        let wnd = 1024;
        let mut c = Connection {
            state: State::SynRcvd,
            send: SendSequenceSpace {
                iss: iss,
                una: iss,
                nxt: iss,
                wnd: wnd,
                up: false,

                wl1: 0,
                wl2: 0,
            },
            recv: RecvSequenceSpace {
                irs: tcp_h.sequence_number(),
                nxt: tcp_h.sequence_number() + 1,
                wnd: tcp_h.window_size(),
                up: false,
            },
            ip: etherparse::Ipv4Header::new(
                0,
                64,
                etherparse::IpTrafficClass::Tcp,
                ip_h.destination().try_into().unwrap(),
                ip_h.source().try_into().unwrap(),
            ),
            tcp: etherparse::TcpHeader::new(
                tcp_h.destination_port(),
                tcp_h.source_port(),
                iss,
                wnd,
            ),

            incoming: Default::default(),
            unacked: Default::default(),

            closed: false,
            closed_at: None,

            timers: Timers {
                send_times: Default::default(),
                srtt: time::Duration::from_secs(1 * 60).as_secs_f64(),
            },
        };

        // needs to start establishing connection -- send syn on
        c.tcp.syn = true;
        c.tcp.ack = false;
        c.write(nic, c.send.nxt, 0)?;
        Ok(Some(c))
    }

    fn write(&mut self, nic: &mut tun_tap::Iface, seq: u32, mut limit: usize) -> io::Result<usize> {
        let mut buf = [0u8; 1500];
        self.tcp.sequence_number = seq;
        self.tcp.acknowledgment_number = self.recv.nxt;

        // TODO: return +1 for SYN/FIN

        println!(
            "write(ack: {}, seq: {}, limit: {}) syn {:?} fin {:?}",
            self.recv.nxt - self.recv.irs,
            seq,
            limit,
            self.tcp.syn,
            self.tcp.fin,
        );

        let mut offset = seq.wrapping_sub(self.send.una) as usize;
        // we need two special case the two 'virtual' bytes SYN and FIN
        if let Some(closed_at) = self.closed_at {
            if seq == closed_at.wrapping_add(1) {
                // trying to write following FIN
                offset = 0;
                limit = 0;
            }
        }

        // println!(
        //     "using offset {} base {} in {:?}",
        //     offset,
        //     self.send.una,
        //     self.unacked.as_slices()
        // );

        let (mut h, mut t) = self.unacked.as_slices();
        if h.len() >= offset {
            h = &h[offset..];
        } else {
            let skipped = h.len();
            h = &[];
            t = &t[(offset - skipped)..];
        }

        let mut max_data = std::cmp::min(limit, h.len() + t.len());
        let size = std::cmp::min(
            buf.len(),
            self.tcp.header_len() as usize + self.ip.header_len() as usize + max_data,
        );

        self.ip
            .set_payload_len(size - self.ip.header_len() as usize);

        // write out the headers
        use std::io::Write;
        let buf_len = buf.len();
        let mut unwritten = &mut buf[..];

        self.ip.write(&mut unwritten);
        let ip_header_ends_at = buf_len - unwritten.len();

        // postpone writing the tcp header because we need the payload as one contiguous slice to calculate the tcp checksum
        unwritten = &mut unwritten[self.tcp.header_len() as usize..];
        let tcp_header_ends_at = buf_len - unwritten.len();

        // write out the payload
        let payload_bytes = {
            let mut written = 0;
            let mut limit = max_data;

            // first, write as much as we can from h
            let p1l = std::cmp::min(limit, h.len());
            written += unwritten.write(&h[..p1l])?;
            limit -= written;

            // then, write more (if we can) from t
            let p2l = std::cmp::min(limit, t.len());
            written += unwritten.write(&t[..p2l])?;
            written
        };
        let payload_ends_at = buf_len - unwritten.len();

        // checksum calculation
        self.tcp.checksum = self
            .tcp
            .calc_checksum_ipv4(&self.ip, &buf[tcp_header_ends_at..payload_ends_at])
            .expect("failed to compute checksum");

        let mut tcp_header_buf = &mut buf[ip_header_ends_at..tcp_header_ends_at];
        self.tcp.write(&mut tcp_header_buf);

        let mut next_seq = seq.wrapping_add(payload_bytes as u32);
        if self.tcp.syn {
            next_seq = next_seq.wrapping_add(1);
            self.tcp.syn = false;
        }
        if self.tcp.fin {
            next_seq = next_seq.wrapping_add(1);
            self.tcp.fin = false;
        }
        if wrapping_lt(self.send.nxt, next_seq) {
            self.send.nxt = next_seq;
        }
        self.timers.send_times.insert(seq, time::Instant::now());

        nic.send(&buf[..payload_ends_at])?;
        Ok(payload_bytes)
    }

    fn send_rst(&mut self, nic: &mut tun_tap::Iface) -> io::Result<()> {
        self.tcp.rst = true;
        // TODO: fix sequence numbers here
        // If the incoming segment has an ACK field, the reset takes its
        // sequence number from the ACK field of the segment, otherwise the
        // reset has sequence number zero and the ACK field is set to the sum
        // of the sequence number and segment length of the incoming segment.
        // The connection remains in the same state.
        //
        // TODO: handle synchronized RST
        // 3.  If the connection is in a synchronized state (ESTABLISHED,
        // FIN-WAIT-1, FIN-WAIT-2, CLOSE-WAIT, CLOSING, LAST-ACK, TIME-WAIT),
        // any unacceptable segment (out of window sequence number or
        // unacceptible acknowledgment number) must elicit only an empty
        // acknowledgment segment containing the current send-sequence number
        // and an acknowledgment indicating the next sequence number expected
        // to be received, and the connection remains in the same state.
        self.tcp.sequence_number = 0;
        self.tcp.acknowledgment_number = 0;
        self.write(nic, self.send.nxt, 0)?;
        Ok(())
    }

    pub(crate) fn on_tick(&mut self, nic: &mut tun_tap::Iface) -> io::Result<()> {
        // TODO: check if it is triggered in closed state
        if let State::FinWait2 | State::TimeWait | State::Closed = self.state {
            // we have shutdown our write side and the other side acked, no need to (re)transmit anything
            return Ok(());
        }

        let nunacked_data = self
            .closed_at
            .unwrap_or(self.send.nxt)
            .wrapping_sub(self.send.una);
        let nunsent_data = self.unacked.len() as u32 - nunacked_data;

        let waited_for = self
            .timers
            .send_times
            .range(self.send.una..)
            .next()
            .map(|t| t.1.elapsed());

        let should_retransmit = if let Some(waited_for) = waited_for {
            waited_for > time::Duration::from_secs(1)
                && waited_for.as_secs_f64() > 1.5 * self.timers.srtt
        } else {
            false
        };

        if should_retransmit {
            // we should retransmit!
            let resend = std::cmp::min(self.unacked.len() as u32, self.send.wnd as u32);
            if resend < self.send.wnd as u32 && self.closed {
                // can we include the FIN?
                self.tcp.fin = true;
                self.closed_at = Some(self.send.una.wrapping_add(self.unacked.len() as u32));
            }

            if (resend == 0) {
                return Ok(());
            };

            self.write(nic, self.send.una, resend as usize)?;
        } else {
            // we should send new data if have new data and space in the window
            if nunsent_data == 0 && self.closed_at.is_some() {
                return Ok(());
            }

            let allowed = self.send.wnd as u32 - nunacked_data;
            if allowed == 0 {
                return Ok(());
            }

            let send = std::cmp::min(nunsent_data, allowed);
            if send < allowed && self.closed && self.closed_at.is_none() {
                self.tcp.fin = true;
                self.closed_at = Some(self.send.una.wrapping_add(self.unacked.len() as u32));
            }

            if (send == 0) {
                return Ok(());
            };

            self.write(nic, self.send.nxt, send as usize)?;
        }

        // if FIN, enter FIN-WAIT-1
        Ok(())
    }

    pub(crate) fn on_packet<'a>(
        &mut self,
        nic: &mut tun_tap::Iface,
        ip_h: etherparse::Ipv4HeaderSlice<'a>,
        tcp_h: etherparse::TcpHeaderSlice<'a>,
        data: &'a [u8],
    ) -> io::Result<Available> {
        // first check that sequence numbers are valid (RFC 793 S3.3)
        //
        // valid segment check okay if it acks at least one byte, which means that at least one of the following is true
        // RCV.NXT =< SEG.SEQ < RCV.NXT + RCV.WND
        // RCV.NXT =< SEG.SEQ + SEG.LEN-1 < RCV.NXT + RCV.WND
        //
        let seqn = tcp_h.sequence_number(); // sequence number
        let mut slen = data.len() as u32;

        if tcp_h.fin() {
            slen += 1;
        };
        if tcp_h.syn() {
            slen += 1;
        };

        let wend = self.recv.nxt.wrapping_add(self.recv.wnd as u32); // window end
        let okay = if slen == 0 {
            // zero-length segment has seperate rules for acceptance
            if self.recv.wnd == 0 {
                if seqn != self.recv.nxt {
                    false
                } else {
                    true
                }
            } else if !is_between_wrapped(self.recv.nxt.wrapping_sub(1), seqn, wend) {
                false
            } else {
                true
            }
        } else {
            if self.recv.wnd == 0 {
                false
            } else if !is_between_wrapped(self.recv.nxt.wrapping_sub(1), seqn, wend)
                && !is_between_wrapped(
                    self.recv.nxt.wrapping_sub(1),
                    seqn.wrapping_add(slen - 1),
                    wend,
                )
            {
                false
            } else {
                true
            }
        };

        if !okay {
            eprintln!("NOT OKAY");
            self.write(nic, self.send.nxt, 0)?;
            return Ok(self.availability());
        }

        if !tcp_h.ack() {
            if tcp_h.syn() {
                // got SYN part of initial handshake
                assert!(data.is_empty());
                self.recv.nxt = seqn.wrapping_add(1);
            }
            return Ok(self.availability());
        }

        let ackn = tcp_h.acknowledgment_number(); // ack number
        if let State::SynRcvd = self.state {
            if is_between_wrapped(
                self.send.una.wrapping_sub(1),
                ackn,
                self.send.nxt.wrapping_add(1),
            ) {
                // must have ACKed our SYN, since we detected at least one ACKed byte
                // and we have only one byte (the SYN)
                self.state = State::Estab;
            } else {
                // TODO: RST : <SEQ=SEH.ACK><CTL=RST>
            }
        }

        if let State::Estab | State::FinWait1 | State::FinWait2 = self.state {
            if is_between_wrapped(self.send.una, ackn, self.send.nxt.wrapping_add(1)) {
                println!(
                    "ack for {} (last: {}); prune in {:?}",
                    ackn, self.send.una, self.unacked
                );
                if !self.unacked.is_empty() {
                    let data_start = if self.send.una == self.send.iss {
                        // send.una hasn't been updated yet with ACK for our SYN, so data starts just beyond it
                        self.send.una.wrapping_add(1)
                    } else {
                        self.send.una
                    };

                    //----------------------------------------------------------------------------------
                    // old version

                    // let acked_data_end =
                    //     std::cmp::min(ackn.wrapping_sub(data_start) as usize, self.unacked.len());
                    // self.unacked.drain(..acked_data_end);

                    // let old = std::mem::replace(&mut self.timers.send_times, BTreeMap::new());

                    // let una = self.send.una;
                    // let mut srtt = &mut self.timers.srtt;
                    // self.timers
                    //     .send_times
                    //     .extend(old.into_iter().filter_map(|(seq, sent)| {
                    //         if is_between_wrapped(una, seq, ackn) {
                    //             *srtt = 0.8 * *srtt + (1.0 - 0.8) * sent.elapsed().as_secs_f64();
                    //             None
                    //         } else {
                    //             Some((seq, sent))
                    //         }
                    //     }));

                    //----------------------------------------------------------------------------------

                    let acked_data_end =
                        std::cmp::min(ackn.wrapping_sub(data_start) as usize, self.unacked.len());
                    self.unacked.drain(..acked_data_end);

                    self.timers.send_times.retain(|&seq, sent| {
                        if is_between_wrapped(self.send.una, seq, ackn) {
                            self.timers.srtt =
                                0.8 * self.timers.srtt + (1.0 - 0.8) * sent.elapsed().as_secs_f64();
                            false
                        } else {
                            true
                        }
                    });

                    //----------------------------------------------------------------------------------
                }
                self.send.una = ackn;
            }

            // TODO: if unacked empty and waiting flush, notify
            // TODO: update window
        }

        // receive ack for out FIN
        if let State::FinWait1 = self.state {
            if let Some(closed_at) = self.closed_at {
                if self.send.una == closed_at.wrapping_add(1) {
                    // our FIN has been ACKed!
                    self.state = State::FinWait2;
                }
            }
        } else if let State::Closing = self.state {
            if let Some(closed_at) = self.closed_at {
                if self.send.una == closed_at.wrapping_add(1) {
                    // our FIN has been ACKed!
                    self.state = State::TimeWait;
                }
            }
        } else if let State::LastAck = self.state {
            if let Some(closed_at) = self.closed_at {
                if self.send.una == closed_at.wrapping_add(1) {
                    // our FIN has been ACKed!
                    self.state = State::Closed;
                }
            }
        }

        if !data.is_empty() {
            if let State::Estab | State::FinWait1 | State::FinWait2 = self.state {
                let mut unread_data_at = self.recv.nxt.wrapping_sub(seqn) as usize;
                if unread_data_at > data.len() {
                    // we must have received a re-transmitted FIN that we ahve already seen
                    // nxt points to beyond the fin, but the fin is not in data
                    assert_eq!(unread_data_at, data.len() + 1);
                    unread_data_at = 0;
                }

                self.incoming.extend(&data[unread_data_at..]);

                /*
                Once the TCP takes responsibility for the data it advances
                RCV.NXT over the data accepted, and adjusts RCV.WND as
                apporopriate to the current buffer availability. The total of
                RCV.NXT and RCV.WND should not be reduced.
                */
                self.recv.nxt = seqn.wrapping_add(data.len() as u32);

                /* Send an acknowledgment of the form: <SEQ=SND.NXT><ACK=RCV.NXT><CTL=ACK> */
                // TODO: maybe just tick to piggyback ack on data??
                self.write(nic, self.send.nxt, 0)?;
            }
        }

        if tcp_h.fin() {
            match self.state {
                State::FinWait2 => {
                    // we are done with connection
                    self.recv.nxt = self.recv.nxt.wrapping_add(1);
                    self.write(nic, self.send.nxt, 0)?;
                    self.state = State::TimeWait;
                }
                State::FinWait1 => {
                    self.recv.nxt = self.recv.nxt.wrapping_add(1);
                    self.write(nic, self.send.nxt, 0)?;
                    self.state = State::Closing;
                }
                State::Estab => {
                    self.recv.nxt = self.recv.nxt.wrapping_add(1);
                    self.write(nic, self.send.nxt, 0)?;
                    self.state = State::TimeWait;
                }
                // we are not expecting FIN flag in other states
                _ => unimplemented!(),
            }
        }

        Ok(self.availability())
    }

    pub(crate) fn close(&mut self) -> io::Result<()> {
        self.closed = true;
        match self.state {
            State::SynRcvd | State::Estab => {
                self.state = State::FinWait1;
            }
            State::CloseWait => {
                self.state = State::LastAck;
            }
            State::FinWait1 | State::FinWait2 | State::Closing | State::LastAck => {}
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::NotConnected,
                    "already closing",
                ))
            }
        }
        Ok(())
    }
}

fn wrapping_lt(lhs: u32, rhs: u32) -> bool {
    // From RFC1323
    // TCP determines if a data segment is 'old' or new by testing
    // weather its sequence number is within 2**31 bytes of left edge
    // of the window, and if it is not disgarding data as old. To
    // insured that new data is never mistakenly considered old and
    // vice-versa, the left edge of the sender's windpw has to be at
    // most 2**31 away from the right edge of the receiver's window
    lhs.wrapping_sub(rhs) > (1 << 31)
}

fn is_between_wrapped(start: u32, x: u32, end: u32) -> bool {
    wrapping_lt(start, x) && wrapping_lt(x, end)
}
