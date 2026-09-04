//! Optional, non-blocking GPU pass timestamps with triple-buffered readback.

use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

const PASS_COUNT: usize = 4;
const QUERY_COUNT: u32 = (PASS_COUNT * 2) as u32;
const QUERY_BYTES: u64 = QUERY_COUNT as u64 * 8;
const READBACK_SLOTS: usize = 3;
const FREE: u8 = 0;
const PENDING: u8 = 1;
const READY: u8 = 2;
const FAILED: u8 = 3;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct GpuPassTimes {
    pub interaction: f32,
    pub shadow: f32,
    pub world: f32,
    pub composite: f32,
}

#[derive(Debug)]
struct ReadbackSlot {
    buffer: wgpu::Buffer,
    state: Arc<AtomicU8>,
    frame_id: u64,
}

#[derive(Debug)]
pub(crate) struct GpuProfiler {
    query_set: wgpu::QuerySet,
    resolve_buffer: wgpu::Buffer,
    readbacks: [ReadbackSlot; READBACK_SLOTS],
    next_slot: usize,
    timestamp_period_ns: f32,
    latest: GpuPassTimes,
    next_frame_id: u64,
    latest_frame_id: u64,
}

impl GpuProfiler {
    pub fn new(device: &wgpu::Device, timestamp_period_ns: f32) -> Self {
        let query_set = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("frame GPU pass timestamps"),
            ty: wgpu::QueryType::Timestamp,
            count: QUERY_COUNT,
        });
        let resolve_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("GPU timestamp resolve"),
            size: QUERY_BYTES,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readbacks = std::array::from_fn(|index| ReadbackSlot {
            buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(match index {
                    0 => "GPU timestamp readback 0",
                    1 => "GPU timestamp readback 1",
                    _ => "GPU timestamp readback 2",
                }),
                size: QUERY_BYTES,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            }),
            state: Arc::new(AtomicU8::new(FREE)),
            frame_id: 0,
        });
        Self {
            query_set,
            resolve_buffer,
            readbacks,
            next_slot: 0,
            timestamp_period_ns,
            latest: GpuPassTimes::default(),
            next_frame_id: 1,
            latest_frame_id: 0,
        }
    }

    pub const fn query_set(&self) -> &wgpu::QuerySet {
        &self.query_set
    }

    /// Collects finished readbacks and reserves a free slot without waiting.
    pub fn begin_frame(&mut self, device: &wgpu::Device) -> Option<usize> {
        let _ = device.poll(wgpu::PollType::Poll);
        self.collect();
        for offset in 0..READBACK_SLOTS {
            let index = (self.next_slot + offset) % READBACK_SLOTS;
            if self.readbacks[index].state.load(Ordering::Acquire) == FREE {
                self.next_slot = (index + 1) % READBACK_SLOTS;
                return Some(index);
            }
        }
        None
    }

    pub fn finish_encoding(&mut self, encoder: &mut wgpu::CommandEncoder, slot_index: usize) {
        let slot = &mut self.readbacks[slot_index];
        slot.frame_id = self.next_frame_id;
        self.next_frame_id += 1;
        slot.state.store(PENDING, Ordering::Release);
        encoder.resolve_query_set(&self.query_set, 0..QUERY_COUNT, &self.resolve_buffer, 0);
        encoder.copy_buffer_to_buffer(&self.resolve_buffer, 0, &slot.buffer, 0, QUERY_BYTES);
        let completion = Arc::clone(&slot.state);
        encoder.map_buffer_on_submit(&slot.buffer, wgpu::MapMode::Read, .., move |result| {
            completion.store(
                if result.is_ok() { READY } else { FAILED },
                Ordering::Release,
            );
        });
    }

    pub const fn latest(&self) -> GpuPassTimes {
        self.latest
    }

    fn collect(&mut self) {
        for slot in &self.readbacks {
            match slot.state.load(Ordering::Acquire) {
                READY => {
                    let mapped = slot.buffer.slice(..).get_mapped_range();
                    let mut ticks = [0_u64; QUERY_COUNT as usize];
                    for (target, bytes) in ticks.iter_mut().zip(mapped.chunks_exact(8)) {
                        *target = u64::from_le_bytes(bytes.try_into().expect("eight-byte chunk"));
                    }
                    drop(mapped);
                    slot.buffer.unmap();
                    // Ring slots complete in frame order, not array-index
                    // order. A batch spanning wraparound must retain the
                    // newest sample instead of overwriting it with old data.
                    if slot.frame_id < self.latest_frame_id {
                        slot.state.store(FREE, Ordering::Release);
                        continue;
                    }
                    self.latest_frame_id = slot.frame_id;
                    let milliseconds = |pass: usize| {
                        ticks[pass * 2 + 1].wrapping_sub(ticks[pass * 2]) as f32
                            * self.timestamp_period_ns
                            * 1.0e-6
                    };
                    self.latest = GpuPassTimes {
                        interaction: milliseconds(0),
                        shadow: milliseconds(1),
                        world: milliseconds(2),
                        composite: milliseconds(3),
                    };
                    slot.state.store(FREE, Ordering::Release);
                }
                FAILED => slot.state.store(FREE, Ordering::Release),
                _ => {}
            }
        }
    }
}
