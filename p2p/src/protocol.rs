// Copyright 2019 The Grin Developers
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::chain;
use crate::conn::{Message, MessageHandler, Tracker};
use crate::core::core::{self, hash::Hash, hash::Hashed, CompactBlock};

use crate::msg::{
	BanReason, GetPeerAddrs, Headers, KernelDataResponse, Locator, Msg, PeerAddrs, Ping, Pong,
	TxHashSetArchive, TxHashSetRequest, Type,
};
use crate::types::{Error, NetAdapter, PeerInfo};
use chrono::prelude::Utc;
use rand::{thread_rng, Rng};
use std::cmp;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tempfile::tempfile;

pub struct Protocol {
	adapter: Arc<dyn NetAdapter>,
	peer_info: PeerInfo,
	state_sync_requested: Arc<AtomicBool>,
}

impl Protocol {
	pub fn new(
		adapter: Arc<dyn NetAdapter>,
		peer_info: PeerInfo,
		state_sync_requested: Arc<AtomicBool>,
	) -> Protocol {
		Protocol {
			adapter,
			peer_info,
			state_sync_requested,
		}
	}
}

impl MessageHandler for Protocol {
	fn consume(
		&self,
		mut msg: Message,
		stopped: Arc<AtomicBool>,
		tracker: Arc<Tracker>,
	) -> Result<Option<Msg>, Error> {
		let adapter = &self.adapter;

		// If we received a msg from a banned peer then log and drop it.
		// If we are getting a lot of these then maybe we are not cleaning
		// banned peers up correctly?
		if adapter.is_banned(self.peer_info.addr) {
			debug!(
				"handler: consume: peer {:?} banned, received: {:?}, dropping.",
				self.peer_info.addr, msg.header.msg_type,
			);
			return Ok(None);
		}

		match msg.header.msg_type {
			Type::Ping => {
				let ping: Ping = msg.body()?;
				adapter.peer_difficulty(
					self.peer_info.addr,
					ping.total_difficulty,
					ping.height,
					ping.local_timestamp,
				);

				Ok(Some(Msg::new(
					Type::Pong,
					Pong {
						total_difficulty: adapter.total_difficulty()?,
						height: adapter.total_height()?,
						local_timestamp: Utc::now().timestamp(),
					},
					self.peer_info.version,
				)?))
			}

			Type::Pong => {
				let pong: Pong = msg.body()?;
				adapter.peer_difficulty(
					self.peer_info.addr,
					pong.total_difficulty,
					pong.height,
					pong.local_timestamp,
				);
				Ok(None)
			}

			Type::BanReason => {
				let ban_reason: BanReason = msg.body()?;
				error!("handle_payload: BanReason {:?}", ban_reason);
				Ok(None)
			}

			Type::TransactionKernel => {
				let h: Hash = msg.body()?;
				debug!(
					"handle_payload: received tx kernel: {}, msg_len: {}",
					h, msg.header.msg_len
				);
				adapter.tx_kernel_received(h, &self.peer_info)?;
				Ok(None)
			}

			Type::GetTransaction => {
				let h: Hash = msg.body()?;
				debug!(
					"handle_payload: GetTransaction: {}, msg_len: {}",
					h, msg.header.msg_len,
				);
				let tx = adapter.get_transaction(h);
				if let Some(tx) = tx {
					Ok(Some(Msg::new(
						Type::Transaction,
						tx,
						self.peer_info.version,
					)?))
				} else {
					Ok(None)
				}
			}

			Type::Transaction => {
				debug!(
					"handle_payload: received tx: msg_len: {}",
					msg.header.msg_len
				);
				let tx: core::Transaction = msg.body()?;
				adapter.transaction_received(tx, false)?;
				Ok(None)
			}

			Type::StemTransaction => {
				debug!(
					"handle_payload: received stem tx: msg_len: {}",
					msg.header.msg_len
				);
				let tx: core::Transaction = msg.body()?;
				adapter.transaction_received(tx, true)?;
				Ok(None)
			}

			Type::GetBlock => {
				let h: Hash = msg.body()?;
				trace!(
					"handle_payload: GetBlock: {}, msg_len: {}",
					h,
					msg.header.msg_len,
				);

				let bo = adapter.get_block(h);
				if let Some(b) = bo {
					return Ok(Some(Msg::new(Type::Block, b, self.peer_info.version)?));
				}
				Ok(None)
			}

			Type::Block => {
				debug!(
					"handle_payload: received block: msg_len: {}",
					msg.header.msg_len
				);
				let b: core::UntrustedBlock = msg.body()?;

				// We default to NONE opts here as we do not know know yet why this block was
				// received.
				// If we requested this block from a peer due to our node syncing then
				// the peer adapter will override opts to reflect this.
				adapter.block_received(b.into(), &self.peer_info, chain::Options::NONE)?;
				Ok(None)
			}

			Type::GetCompactBlock => {
				let h: Hash = msg.body()?;
				if let Some(b) = adapter.get_block(h) {
					let cb: CompactBlock = b.into();
					Ok(Some(Msg::new(
						Type::CompactBlock,
						cb,
						self.peer_info.version,
					)?))
				} else {
					Ok(None)
				}
			}

			Type::CompactBlock => {
				debug!(
					"handle_payload: received compact block: msg_len: {}",
					msg.header.msg_len
				);
				let b: core::UntrustedCompactBlock = msg.body()?;

				adapter.compact_block_received(b.into(), &self.peer_info)?;
				Ok(None)
			}

			Type::GetHeaders => {
				// load headers from the locator
				let loc: Locator = msg.body()?;
				let headers = adapter.locate_headers(&loc.hashes)?;

				// serialize and send all the headers over
				Ok(Some(Msg::new(
					Type::Headers,
					Headers { headers },
					self.peer_info.version,
				)?))
			}

			// "header first" block propagation - if we have not yet seen this block
			// we can go request it from some of our peers
			Type::Header => {
				let header: core::UntrustedBlockHeader = msg.body()?;
				adapter.header_received(header.into(), &self.peer_info)?;
				Ok(None)
			}

			Type::Headers => {
				let mut total_bytes_read = 0;

				// Read the count (u16) so we now how many headers to read.
				let (count, bytes_read): (u16, _) = msg.streaming_read()?;
				total_bytes_read += bytes_read;

				// Read chunks of headers off the stream and pass them off to the adapter.
				let chunk_size = 128;
				for chunk in (0..count).collect::<Vec<_>>().chunks(chunk_size) {
					let mut headers = vec![];
					for _ in chunk {
						let (header, bytes_read) =
							msg.streaming_read::<core::UntrustedBlockHeader>()?;
						headers.push(header.into());
						total_bytes_read += bytes_read;
					}
					adapter.headers_received(&headers, &self.peer_info)?;
				}

				// Now check we read the correct total number of bytes off the stream.
				if total_bytes_read != msg.header.msg_len {
					return Err(Error::MsgLen);
				}

				Ok(None)
			}

			Type::GetPeerAddrs => {
				let get_peers: GetPeerAddrs = msg.body()?;
				let peers = adapter.find_peer_addrs(get_peers.capabilities);
				Ok(Some(Msg::new(
					Type::PeerAddrs,
					PeerAddrs { peers },
					self.peer_info.version,
				)?))
			}

			Type::PeerAddrs => {
				let peer_addrs: PeerAddrs = msg.body()?;
				adapter.peer_addrs_received(peer_addrs.peers);
				Ok(None)
			}

			Type::KernelDataRequest => {
				debug!("handle_payload: kernel_data_request");
				let kernel_data = self.adapter.kernel_data_read()?;
				let bytes = kernel_data.metadata()?.len();
				let kernel_data_response = KernelDataResponse { bytes };
				let mut response = Msg::new(
					Type::KernelDataResponse,
					&kernel_data_response,
					self.peer_info.version,
				)?;
				response.add_attachment(kernel_data);
				Ok(Some(response))
			}

			Type::KernelDataResponse => {
				let response: KernelDataResponse = msg.body()?;
				debug!(
					"handle_payload: kernel_data_response: bytes: {}",
					response.bytes
				);

				let mut writer = BufWriter::new(tempfile()?);

				let total_size = response.bytes as usize;
				let mut remaining_size = total_size;

				while remaining_size > 0 {
					let size = msg.copy_attachment(remaining_size, &mut writer)?;
					remaining_size = remaining_size.saturating_sub(size);

					// Increase received bytes quietly (without affecting the counters).
					// Otherwise we risk banning a peer as "abusive".
					tracker.inc_quiet_received(size as u64);
				}

				// Remember to seek back to start of the file as the caller is likely
				// to read this file directly without reopening it.
				writer.seek(SeekFrom::Start(0))?;

				let mut file = writer.into_inner().map_err(|_| Error::Internal)?;

				debug!(
					"handle_payload: kernel_data_response: file size: {}",
					file.metadata().unwrap().len()
				);

				self.adapter.kernel_data_write(&mut file)?;

				Ok(None)
			}

			Type::TxHashSetRequest => {
				let sm_req: TxHashSetRequest = msg.body()?;
				debug!(
					"handle_payload: txhashset req for {} at {}",
					sm_req.hash, sm_req.height
				);

				let txhashset_header = self.adapter.txhashset_archive_header()?;
				let txhashset_header_hash = txhashset_header.hash();
				let txhashset = self.adapter.txhashset_read(txhashset_header_hash);

				if let Some(txhashset) = txhashset {
					let file_sz = txhashset.reader.metadata()?.len();
					let mut resp = Msg::new(
						Type::TxHashSetArchive,
						&TxHashSetArchive {
							height: txhashset_header.height as u64,
							hash: txhashset_header_hash,
							bytes: file_sz,
						},
						self.peer_info.version,
					)?;
					resp.add_attachment(txhashset.reader);
					Ok(Some(resp))
				} else {
					Ok(None)
				}
			}

			Type::TxHashSetArchive => {
				let sm_arch: TxHashSetArchive = msg.body()?;
				debug!(
					"handle_payload: txhashset archive for {} at {}. size={}",
					sm_arch.hash, sm_arch.height, sm_arch.bytes,
				);
				if !self.adapter.txhashset_receive_ready() {
					error!(
						"handle_payload: txhashset archive received but SyncStatus not on TxHashsetDownload",
					);
					return Err(Error::BadMessage);
				}
				if !self.state_sync_requested.load(Ordering::Relaxed) {
					error!("handle_payload: txhashset archive received but from the wrong peer",);
					return Err(Error::BadMessage);
				}
				// Update the sync state requested status
				self.state_sync_requested.store(false, Ordering::Relaxed);

				let download_start_time = Utc::now();
				self.adapter
					.txhashset_download_update(download_start_time, 0, sm_arch.bytes);

				let nonce: u32 = thread_rng().gen_range(0, 1_000_000);
				let tmp = self.adapter.get_tmpfile_pathname(format!(
					"txhashset-{}-{}.zip",
					download_start_time.timestamp(),
					nonce
				));
				let mut now = Instant::now();
				let mut save_txhashset_to_file = |file| -> Result<(), Error> {
					let mut tmp_zip =
						BufWriter::new(OpenOptions::new().write(true).create_new(true).open(file)?);
					let total_size = sm_arch.bytes as usize;
					let mut downloaded_size: usize = 0;
					let mut request_size = cmp::min(48_000, total_size);
					while request_size > 0 {
						let size = msg.copy_attachment(request_size, &mut tmp_zip)?;
						downloaded_size += size;
						request_size = cmp::min(48_000, total_size - downloaded_size);
						self.adapter.txhashset_download_update(
							download_start_time,
							downloaded_size as u64,
							total_size as u64,
						);
						if now.elapsed().as_secs() > 10 {
							now = Instant::now();
							debug!(
								"handle_payload: txhashset archive: {}/{}",
								downloaded_size, total_size
							);
						}
						// Increase received bytes quietly (without affecting the counters).
						// Otherwise we risk banning a peer as "abusive".
						tracker.inc_quiet_received(size as u64);

						// check the close channel
						if stopped.load(Ordering::Relaxed) {
							debug!("stopping txhashset download early");
							return Err(Error::ConnectionClose);
						}
					}
					debug!(
						"handle_payload: txhashset archive: {}/{} ... DONE",
						downloaded_size, total_size
					);
					tmp_zip
						.into_inner()
						.map_err(|_| Error::Internal)?
						.sync_all()?;
					Ok(())
				};

				if let Err(e) = save_txhashset_to_file(tmp.clone()) {
					error!(
						"handle_payload: txhashset archive save to file fail. err={:?}",
						e
					);
					return Err(e);
				}

				trace!(
					"handle_payload: txhashset archive save to file {:?} success",
					tmp,
				);

				let tmp_zip = File::open(tmp.clone())?;
				let res = self
					.adapter
					.txhashset_write(sm_arch.hash, tmp_zip, &self.peer_info)?;

				debug!(
					"handle_payload: txhashset archive for {} at {}, DONE. Data Ok: {}",
					sm_arch.hash, sm_arch.height, !res
				);

				if let Err(e) = fs::remove_file(tmp.clone()) {
					warn!("fail to remove tmp file: {:?}. err: {}", tmp, e);
				}

				Ok(None)
			}
			Type::Error | Type::Hand | Type::Shake => {
				debug!("Received an unexpected msg: {:?}", msg.header.msg_type);
				Ok(None)
			}
		}
	}
}
