pub use debugid;

use debugid::{CodeId, DebugId};
use fxhash::FxHasher;
use serde::ser::{Serialize, SerializeMap, SerializeSeq, Serializer};
use serde_json::{json, Value};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::hash::BuildHasherDefault;
use std::ops::{Deref, Range};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod markers;
mod timestamp;

pub use markers::*;
pub use timestamp::*;

type FastHashMap<K, V> = HashMap<K, V, BuildHasherDefault<FxHasher>>;

#[derive(Debug)]
pub struct Profile {
    product: String,
    interval: Duration,
    libs: GlobalLibTable,
    processes: Vec<Process>, // append-only for stable ProcessHandles
    threads: Vec<Thread>,    // append-only for stable ThreadHandles
    reference_timestamp: ReferenceTimestamp,
    string_table: GlobalStringTable,
    marker_schemas: FastHashMap<&'static str, MarkerSchema>,
}

#[derive(Debug, Clone, Copy, PartialOrd, PartialEq)]
pub struct ReferenceTimestamp {
    ms_since_unix_epoch: f64,
}

impl ReferenceTimestamp {
    pub fn from_duration_since_unix_epoch(duration: Duration) -> Self {
        Self::from_millis_since_unix_epoch(duration.as_secs_f64() * 1000.0)
    }

    pub fn from_millis_since_unix_epoch(ms_since_unix_epoch: f64) -> Self {
        Self {
            ms_since_unix_epoch,
        }
    }

    pub fn from_system_time(system_time: SystemTime) -> Self {
        Self::from_duration_since_unix_epoch(system_time.duration_since(UNIX_EPOCH).unwrap())
    }
}

impl Serialize for ReferenceTimestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.ms_since_unix_epoch.serialize(serializer)
    }
}

#[derive(Debug, Clone, Copy, PartialOrd, Ord, PartialEq, Eq, Hash)]
pub struct InstantTimestampMaker {
    reference_instant: Instant,
}

impl From<Instant> for InstantTimestampMaker {
    fn from(instant: Instant) -> Self {
        Self {
            reference_instant: instant,
        }
    }
}

impl InstantTimestampMaker {
    pub fn make_ts(&self, instant: Instant) -> Timestamp {
        Timestamp::from_duration_since_reference(
            instant.saturating_duration_since(self.reference_instant),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialOrd, Ord, PartialEq, Eq, Hash)]
pub struct CpuDelta {
    micros: u64,
}

impl From<Duration> for CpuDelta {
    fn from(duration: Duration) -> Self {
        Self {
            micros: duration.as_micros() as u64,
        }
    }
}

impl CpuDelta {
    const ZERO: Self = Self { micros: 0 };

    pub fn from_nanos(nanos: u64) -> Self {
        Self {
            micros: nanos / 1000,
        }
    }

    pub fn from_micros(micros: u64) -> Self {
        Self { micros }
    }

    pub fn from_millis(millis: f64) -> Self {
        Self {
            micros: (millis * 1_000.0) as u64,
        }
    }

    pub fn is_zero(&self) -> bool {
        self.micros == 0
    }
}

impl Serialize for CpuDelta {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // CPU deltas are serialized as float microseconds, because
        // we set profile.meta.sampleUnits.threadCPUDelta to "µs".
        self.micros.serialize(serializer)
    }
}

/// A library ("binary" / "module" / "DSO") which is loaded into a process.
/// This can be the main executable file or a dynamic library, or any other
/// mapping of executable memory.
///
/// Library information makes after-the-fact symbolication possible: The
/// profile JSON contains raw code addresses, and then the symbols for these
/// addresses get resolved later.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LibraryInfo {
    /// The "actual virtual memory address", in the address space of the process,
    /// where this library's base address is located. The base address is the
    /// address which "relative addresses" are relative to.
    ///
    /// For ELF binaries, the base address is equal to the "image bias", i.e. the
    /// offset that is added to the virtual memory addresses as stated in the
    /// library file (SVMAs, "stated virtual memory addresses"). In other words,
    /// the base AVMA corresponds to SVMA zero.
    ///
    /// For mach-O binaries, the base address is the start of the `__TEXT` segment.
    ///
    /// For Windows binaries, the base address is the image load address.
    pub base_avma: u64,
    /// The address range that this mapping occupies in the virtual memory
    /// address space of the process. AVMA = "actual virtual memory address"
    pub avma_range: Range<u64>,
    /// The name of this library that should be displayed in the profiler.
    /// Usually this is the filename of the binary, but it could also be any other
    /// name, such as "[kernel.kallsyms]" or "[vdso]".
    pub name: String,
    /// The debug name of this library which should be used when looking up symbols.
    /// On Windows this is the filename of the PDB file, on other platforms it's
    /// usually the same as the filename of the binary.
    pub debug_name: String,
    /// The absolute path to the binary file.
    pub path: String,
    /// The absolute path to the debug file. On Linux and macOS this is the same as
    /// the path to the binary file. On Windows this is the path to the PDB file.
    pub debug_path: String,
    /// The debug ID of the library. This lets symbolication confirm that it's
    /// getting symbols for the right file, and it can sometimes allow obtaining a
    /// symbol file from a symbol server.
    pub debug_id: DebugId,
    /// The code ID of the library. This lets symbolication confirm that it's
    /// getting symbols for the right file, and it can sometimes allow obtaining a
    /// symbol file from a symbol server.
    pub code_id: Option<CodeId>,
    /// An optional string with the CPU arch of this library, for example "x86_64",
    /// "arm64", or "arm64e". Historically, this was used on macOS to find the
    /// correct sub-binary in a fat binary. But we now use the debug_id for that
    /// purpose.
    pub arch: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialOrd, Ord, PartialEq, Eq, Hash)]
pub struct ProcessHandle(usize);

#[derive(Debug, Clone, Copy, PartialOrd, Ord, PartialEq, Eq, Hash)]
pub struct ThreadHandle(usize);

#[derive(Debug, Clone, Copy, PartialOrd, Ord, PartialEq, Eq, Hash)]
struct ProcessLibIndex(usize);

#[derive(Debug, Clone, Copy, PartialOrd, Ord, PartialEq, Eq, Hash)]
pub struct GlobalLibIndex(usize);

#[derive(Debug, Clone, Copy, PartialOrd, Ord, PartialEq, Eq, Hash)]
struct GlobalStringIndex(StringIndex);

#[derive(Debug, Clone, Copy, PartialOrd, Ord, PartialEq, Eq, Hash)]
pub struct StringHandle(GlobalStringIndex);

#[derive(Debug, Clone, Copy, PartialOrd, Ord, PartialEq, Eq, Hash)]
struct ThreadInternalStringIndex(StringIndex);

#[derive(Debug, Clone, Copy, PartialOrd, Ord, PartialEq, Eq, Hash)]
struct StringIndex(u32);

#[derive(Debug, Clone, PartialOrd, Ord, PartialEq, Eq)]
pub enum Frame {
    /// A code address taken from the instruction pointer
    InstructionPointer(u64),
    /// A code address taken from a return address
    ReturnAddress(u64),
    /// A string, containing an index returned by Profile::intern_string
    Label(StringHandle),
}

impl Profile {
    pub fn new(product: &str, reference_timestamp: ReferenceTimestamp, interval: Duration) -> Self {
        Profile {
            interval,
            product: product.to_string(),
            threads: Vec::new(),
            libs: GlobalLibTable::new(),
            reference_timestamp,
            processes: Vec::new(),
            string_table: GlobalStringTable::new(),
            marker_schemas: FastHashMap::default(),
        }
    }

    pub fn set_interval(&mut self, interval: Duration) {
        self.interval = interval;
    }

    pub fn set_reference_timestamp(&mut self, reference_timestamp: ReferenceTimestamp) {
        self.reference_timestamp = reference_timestamp;
    }

    pub fn set_product(&mut self, product: &str) {
        self.product = product.to_string();
    }

    pub fn add_process(&mut self, name: &str, pid: u32, start_time: Timestamp) -> ProcessHandle {
        let handle = ProcessHandle(self.processes.len());
        self.processes.push(Process {
            pid,
            threads: Vec::new(),
            sorted_lib_ranges: Vec::new(),
            used_lib_map: FastHashMap::default(),
            libs: Vec::new(),
            start_time,
            end_time: None,
            name: name.to_owned(),
        });
        handle
    }

    pub fn set_process_start_time(&mut self, process: ProcessHandle, start_time: Timestamp) {
        self.processes[process.0].start_time = start_time;
    }

    pub fn set_process_end_time(&mut self, process: ProcessHandle, end_time: Timestamp) {
        self.processes[process.0].end_time = Some(end_time);
    }

    pub fn set_process_name(&mut self, process: ProcessHandle, name: &str) {
        self.processes[process.0].name = name.to_string();
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_lib(&mut self, process: ProcessHandle, library: LibraryInfo) {
        self.processes[process.0].add_lib(library);
    }

    pub fn unload_lib(&mut self, process: ProcessHandle, base_address: u64) {
        self.processes[process.0].unload_lib(base_address);
    }

    pub fn add_thread(
        &mut self,
        process: ProcessHandle,
        tid: u32,
        start_time: Timestamp,
        is_main: bool,
    ) -> ThreadHandle {
        let handle = ThreadHandle(self.threads.len());
        self.threads.push(Thread {
            process,
            tid,
            name: None,
            start_time,
            end_time: None,
            is_main,
            stack_table: StackTable::new(),
            frame_table_and_func_table: FrameTableAndFuncTable::new(),
            samples: SampleTable::new(),
            markers: MarkerTable::new(),
            resources: ResourceTable::new(),
            string_table: ThreadStringTable::new(),
            last_sample_stack: None,
            last_sample_was_zero_cpu: false,
        });
        self.processes[process.0].threads.push(handle);
        handle
    }

    pub fn set_thread_name(&mut self, thread: ThreadHandle, name: &str) {
        self.threads[thread.0].name = Some(name.to_string());
    }

    pub fn set_thread_start_time(&mut self, thread: ThreadHandle, start_time: Timestamp) {
        self.threads[thread.0].start_time = start_time;
    }

    pub fn set_thread_end_time(&mut self, thread: ThreadHandle, end_time: Timestamp) {
        self.threads[thread.0].end_time = Some(end_time);
    }

    pub fn intern_string(&mut self, s: &str) -> StringHandle {
        StringHandle(self.string_table.index_for_string(s))
    }

    pub fn add_sample(
        &mut self,
        thread: ThreadHandle,
        timestamp: Timestamp,
        frames: impl Iterator<Item = Frame>,
        cpu_delta: CpuDelta,
        weight: i32,
    ) {
        let stack_index = self.stack_index_for_frames(thread, frames);
        self.threads[thread.0].add_sample(timestamp, stack_index, cpu_delta, weight);
    }

    pub fn add_sample_same_stack_zero_cpu(
        &mut self,
        thread: ThreadHandle,
        timestamp: Timestamp,
        weight: i32,
    ) {
        self.threads[thread.0].add_sample_same_stack_zero_cpu(timestamp, weight);
    }

    /// Main marker API to add a new marker to profiler buffer.
    pub fn add_marker<T: ProfilerMarker>(
        &mut self,
        thread: ThreadHandle,
        name: &str,
        marker: T,
        timing: MarkerTiming,
    ) {
        self.marker_schemas
            .entry(T::MARKER_TYPE_NAME)
            .or_insert_with(T::schema);
        let name_string_index = self.threads[thread.0].string_table.index_for_string(name);
        self.threads[thread.0].markers.add_marker(
            name_string_index,
            timing,
            marker.json_marker_data(),
        );
    }

    // frames is ordered from caller to callee, i.e. root function first, pc last
    fn stack_index_for_frames(
        &mut self,
        thread: ThreadHandle,
        frames: impl Iterator<Item = Frame>,
    ) -> Option<usize> {
        let process = self.threads[thread.0].process;
        let mut prefix = None;
        for frame in frames {
            let internal_frame = match frame {
                Frame::InstructionPointer(ip) => self.convert_address(process, ip),
                Frame::ReturnAddress(ra) => self.convert_address(process, ra.saturating_sub(1)),
                Frame::Label(string_index) => {
                    let thread_string_index = self.convert_string_index(thread, string_index.0);
                    InternalFrame::Label(thread_string_index)
                }
            };
            let frame_index = self.frame_index_for_frame(thread, internal_frame);
            prefix = Some(
                self.threads[thread.0]
                    .stack_table
                    .index_for_stack(prefix, frame_index),
            );
        }
        prefix
    }

    fn convert_address(&mut self, process: ProcessHandle, address: u64) -> InternalFrame {
        let ranges = &self.processes[process.0].sorted_lib_ranges[..];
        let index = match ranges.binary_search_by_key(&address, |r| r.start) {
            Err(0) => return InternalFrame::UnknownAddress(address),
            Ok(exact_match) => exact_match,
            Err(insertion_index) => {
                let range_index = insertion_index - 1;
                if address < ranges[range_index].end {
                    range_index
                } else {
                    return InternalFrame::UnknownAddress(address);
                }
            }
        };
        let range = &ranges[index];
        let process_lib = range.lib_index;
        let relative_address = (address - range.base) as u32;
        let lib_index = self.processes[process.0].convert_lib_index(process_lib, &mut self.libs);
        InternalFrame::AddressInLib(relative_address, lib_index)
    }

    fn convert_string_index(
        &mut self,
        thread: ThreadHandle,
        index: GlobalStringIndex,
    ) -> ThreadInternalStringIndex {
        self.threads[thread.0]
            .string_table
            .index_for_global_string(index, &self.string_table)
    }

    fn frame_index_for_frame(&mut self, thread: ThreadHandle, frame: InternalFrame) -> usize {
        self.threads[thread.0].frame_index_for_frame(frame, &self.libs)
    }
}

impl Serialize for Profile {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut marker_schemas: Vec<MarkerSchema> = self.marker_schemas.values().cloned().collect();
        marker_schemas.sort_by_key(|schema| schema.type_name);

        let meta = json!({
            "categories": [
                {
                    "name": "Regular",
                    "color": "blue",
                    "subcategories": ["Other"],
                },
                {
                    "name": "Other",
                    "color": "grey",
                    "subcategories": ["Other"],
                }
            ],
            "debug": false,
            "extensions": {
                "length": 0,
                "baseURL": [],
                "id": [],
                "name": [],
            },
            "interval": self.interval.as_secs_f64() * 1000.0,
            "markerSchema": marker_schemas,
            "preprocessedProfileVersion": 41,
            "processType": 0,
            "product": self.product,
            "sampleUnits": {
                "time": "ms",
                "eventDelay": "ms",
                "threadCPUDelta": "µs"
            },
            "startTime": self.reference_timestamp,
            "symbolicated": false,
            "pausedRanges": [],
            "version": 24,
        });

        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("meta", &meta)?;
        map.serialize_entry("libs", &self.libs)?;
        map.serialize_entry("threads", &SerializableProfileThreadsProperty(self))?;
        map.serialize_entry("pages", &[] as &[()])?;
        map.serialize_entry("profilerOverhead", &[] as &[()])?;
        map.serialize_entry("counters", &[] as &[()])?;
        map.end()
    }
}

struct SerializableProfileThreadsProperty<'a>(&'a Profile);

impl<'a> Serialize for SerializableProfileThreadsProperty<'a> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // The processed profile format has all threads from all processes in a flattened threads list.
        // Each thread duplicates some information about its process, which allows the Firefox Profiler
        // UI to group threads from the same process.

        let mut seq = serializer.serialize_seq(Some(self.0.threads.len()))?;

        let mut sorted_processes: Vec<_> = (0..self.0.processes.len()).map(ProcessHandle).collect();
        sorted_processes.sort_by(|a_handle, b_handle| {
            let a = &self.0.processes[a_handle.0];
            let b = &self.0.processes[b_handle.0];
            if let Some(ordering) = a.start_time.partial_cmp(&b.start_time) {
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
            a.pid.cmp(&b.pid)
        });

        for process in sorted_processes {
            let mut sorted_threads = self.0.processes[process.0].threads.clone();
            sorted_threads.sort_by(|a_handle, b_handle| {
                let a = &self.0.threads[a_handle.0];
                let b = &self.0.threads[b_handle.0];
                if let Some(ordering) = a.get_start_time().partial_cmp(&b.get_start_time()) {
                    if ordering != Ordering::Equal {
                        return ordering;
                    }
                }
                let ordering = a.get_name().cmp(&b.get_name());
                if ordering != Ordering::Equal {
                    return ordering;
                }
                a.get_tid().cmp(&b.get_tid())
            });

            for thread in sorted_threads {
                seq.serialize_element(&SerializableProfileThread(self.0, thread))?;
            }
        }

        seq.end()
    }
}

#[derive(Debug, Clone, PartialOrd, Ord, PartialEq, Eq, Hash)]
enum InternalFrame {
    UnknownAddress(u64),
    AddressInLib(u32, GlobalLibIndex),
    Label(ThreadInternalStringIndex),
}

#[derive(Debug)]
struct GlobalLibTable {
    libs: Vec<Lib>, // append-only for stable GlobalLibIndexes
    lib_map: FastHashMap<Lib, GlobalLibIndex>,
}

impl GlobalLibTable {
    pub fn new() -> Self {
        Self {
            libs: Vec::new(),
            lib_map: FastHashMap::default(),
        }
    }

    pub fn index_for_lib(&mut self, lib: Lib) -> GlobalLibIndex {
        let libs = &mut self.libs;
        *self.lib_map.entry(lib.clone()).or_insert_with(|| {
            let index = GlobalLibIndex(libs.len());
            libs.push(lib);
            index
        })
    }

    pub fn lib_name(&self, index: GlobalLibIndex) -> &str {
        &self.libs[index.0].name
    }
}

impl Serialize for GlobalLibTable {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.libs.serialize(serializer)
    }
}

#[derive(Debug)]
struct Process {
    pid: u32,
    name: String,
    threads: Vec<ThreadHandle>,
    start_time: Timestamp,
    end_time: Option<Timestamp>,
    libs: Vec<Lib>,
    sorted_lib_ranges: Vec<ProcessLibRange>,
    used_lib_map: FastHashMap<ProcessLibIndex, GlobalLibIndex>,
}

impl Process {
    pub fn convert_lib_index(
        &mut self,
        process_lib: ProcessLibIndex,
        global_libs: &mut GlobalLibTable,
    ) -> GlobalLibIndex {
        let libs = &self.libs;
        *self
            .used_lib_map
            .entry(process_lib)
            .or_insert_with(|| global_libs.index_for_lib(libs[process_lib.0].clone()))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_lib(&mut self, lib: LibraryInfo) {
        let lib_index = ProcessLibIndex(self.libs.len());
        self.libs.push(Lib {
            name: lib.name,
            debug_name: lib.debug_name,
            path: lib.path,
            debug_path: lib.debug_path,
            arch: lib.arch,
            debug_id: lib.debug_id,
            code_id: lib.code_id,
        });

        let insertion_index = match self
            .sorted_lib_ranges
            .binary_search_by_key(&lib.avma_range.start, |r| r.start)
        {
            Ok(i) => {
                // We already have a library mapping at this address.
                // Not sure how to best deal with it. Ideally it wouldn't happen. Let's just remove this mapping.
                self.sorted_lib_ranges.remove(i);
                i
            }
            Err(i) => i,
        };

        self.sorted_lib_ranges.insert(
            insertion_index,
            ProcessLibRange {
                lib_index,
                base: lib.base_avma,
                start: lib.avma_range.start,
                end: lib.avma_range.end,
            },
        );
    }

    pub fn unload_lib(&mut self, base_address: u64) {
        self.sorted_lib_ranges.retain(|r| r.base != base_address);
    }
}

#[derive(Debug, Clone, PartialEq, PartialOrd, Ord, Eq)]
struct ProcessLibRange {
    start: u64,
    end: u64,
    lib_index: ProcessLibIndex,
    base: u64,
}

#[derive(Debug)]
struct Thread {
    process: ProcessHandle,
    tid: u32,
    name: Option<String>,
    start_time: Timestamp,
    end_time: Option<Timestamp>,
    is_main: bool,
    stack_table: StackTable,
    frame_table_and_func_table: FrameTableAndFuncTable,
    samples: SampleTable,
    markers: MarkerTable,
    resources: ResourceTable,
    string_table: ThreadStringTable,
    last_sample_stack: Option<usize>,
    last_sample_was_zero_cpu: bool,
}

impl Thread {
    fn frame_index_for_frame(
        &mut self,
        frame: InternalFrame,
        global_libs: &GlobalLibTable,
    ) -> usize {
        self.frame_table_and_func_table.index_for_frame(
            &mut self.string_table,
            &mut self.resources,
            global_libs,
            frame,
        )
    }

    pub fn add_sample(
        &mut self,
        timestamp: Timestamp,
        stack_index: Option<usize>,
        cpu_delta: CpuDelta,
        weight: i32,
    ) {
        self.samples.sample_weights.push(weight);
        self.samples.sample_timestamps.push(timestamp);
        self.samples.sample_stack_indexes.push(stack_index);
        self.samples.sample_cpu_deltas.push(cpu_delta);
        self.last_sample_stack = stack_index;
        self.last_sample_was_zero_cpu = cpu_delta == CpuDelta::ZERO;
    }

    pub fn add_sample_same_stack_zero_cpu(&mut self, timestamp: Timestamp, weight: i32) {
        if self.last_sample_was_zero_cpu {
            *self.samples.sample_weights.last_mut().unwrap() += weight;
            *self.samples.sample_timestamps.last_mut().unwrap() = timestamp;
        } else {
            let stack_index = self.last_sample_stack;
            self.samples.sample_weights.push(weight);
            self.samples.sample_timestamps.push(timestamp);
            self.samples.sample_stack_indexes.push(stack_index);
            self.samples.sample_cpu_deltas.push(CpuDelta::ZERO);
            self.last_sample_was_zero_cpu = true;
        }
    }

    pub fn get_start_time(&self) -> Timestamp {
        self.start_time
    }

    pub fn get_name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn get_tid(&self) -> u32 {
        self.tid
    }
}

struct SerializableProfileThread<'a>(&'a Profile, ThreadHandle);

impl<'a> Serialize for SerializableProfileThread<'a> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let thread_handle = self.1;
        let thread = &self.0.threads[thread_handle.0];

        let process_handle = thread.process;
        let process = &self.0.processes[process_handle.0];

        let process_start_time = process.start_time;
        let process_end_time = process.end_time;
        let process_name = &process.name;
        let pid = process.pid;

        let tid = thread.tid;

        let thread_name = if thread.is_main {
            // https://github.com/firefox-devtools/profiler/issues/2508
            "GeckoMain".to_string()
        } else if let Some(name) = &thread.name {
            name.clone()
        } else {
            format!("Thread <{}>", tid)
        };

        let thread_register_time = thread.start_time;
        let thread_unregister_time = thread.end_time;

        let native_symbols = json!({
            "length": 0,
            "address": [],
            "libIndex": [],
            "name": [],
        });

        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry(
            "frameTable",
            &thread.frame_table_and_func_table.as_frame_table(),
        )?;
        map.serialize_entry(
            "funcTable",
            &thread.frame_table_and_func_table.as_func_table(),
        )?;
        map.serialize_entry("markers", &thread.markers)?;
        map.serialize_entry("name", &thread_name)?;
        map.serialize_entry("nativeSymbols", &native_symbols)?;
        map.serialize_entry("pausedRanges", &[] as &[()])?;
        map.serialize_entry("pid", &pid)?;
        map.serialize_entry("processName", process_name)?;
        map.serialize_entry("processShutdownTime", &process_end_time)?;
        map.serialize_entry("processStartupTime", &process_start_time)?;
        map.serialize_entry("processType", &"default")?;
        map.serialize_entry("registerTime", &thread_register_time)?;
        map.serialize_entry("resourceTable", &thread.resources)?;
        map.serialize_entry("samples", &thread.samples)?;
        map.serialize_entry("stackTable", &thread.stack_table)?;
        map.serialize_entry("stringArray", &thread.string_table)?;
        map.serialize_entry("tid", &thread.tid)?;
        map.serialize_entry("unregisterTime", &thread_unregister_time)?;
        map.end()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Lib {
    name: String,
    debug_name: String,
    path: String,
    debug_path: String,
    arch: Option<String>,
    debug_id: DebugId,
    code_id: Option<CodeId>,
}

impl<'a> Serialize for Lib {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let breakpad_id = self.debug_id.breakpad().to_string();
        let code_id = self.code_id.as_ref().map(|cid| cid.to_string());
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("name", &self.name)?;
        map.serialize_entry("path", &self.path)?;
        map.serialize_entry("debugName", &self.debug_name)?;
        map.serialize_entry("debugPath", &self.debug_path)?;
        map.serialize_entry("breakpadId", &breakpad_id)?;
        map.serialize_entry("codeId", &code_id)?;
        map.serialize_entry("arch", &self.arch)?;
        map.end()
    }
}

#[derive(Debug, Clone, Default)]
struct StackTable {
    stack_prefixes: Vec<Option<usize>>,
    stack_frames: Vec<usize>,

    // (parent stack, frame_index) -> stack index
    index: FastHashMap<(Option<usize>, usize), usize>,
}

impl StackTable {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn index_for_stack(&mut self, prefix: Option<usize>, frame: usize) -> usize {
        match self.index.get(&(prefix, frame)) {
            Some(stack) => *stack,
            None => {
                let stack = self.stack_prefixes.len();
                self.stack_prefixes.push(prefix);
                self.stack_frames.push(frame);
                self.index.insert((prefix, frame), stack);
                stack
            }
        }
    }
}

impl Serialize for StackTable {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let len = self.stack_prefixes.len();
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("length", &len)?;
        map.serialize_entry("prefix", &self.stack_prefixes)?;
        map.serialize_entry("frame", &self.stack_frames)?;
        map.serialize_entry("category", &SerializableSingleValueColumn(0, len))?;
        map.serialize_entry("subcategory", &SerializableSingleValueColumn(0, len))?;
        map.end()
    }
}

#[derive(Debug, Clone, Default)]
struct FrameTableAndFuncTable {
    // We create one func for every frame.
    frame_addresses: Vec<Option<u32>>,
    func_names: Vec<ThreadInternalStringIndex>,
    func_resources: Vec<Option<ResourceIndex>>,

    // address -> frame index
    index: FastHashMap<InternalFrame, usize>,
}

impl FrameTableAndFuncTable {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn index_for_frame(
        &mut self,
        string_table: &mut ThreadStringTable,
        resource_table: &mut ResourceTable,
        global_libs: &GlobalLibTable,
        frame: InternalFrame,
    ) -> usize {
        let frame_addresses = &mut self.frame_addresses;
        let func_names = &mut self.func_names;
        let func_resources = &mut self.func_resources;
        *self.index.entry(frame.clone()).or_insert_with(|| {
            let frame_index = frame_addresses.len();
            let (address, location_string_index, resource) = match frame {
                InternalFrame::UnknownAddress(address) => {
                    let location_string = format!("0x{:x}", address);
                    let s = string_table.index_for_string(&location_string);
                    (None, s, None)
                }
                InternalFrame::AddressInLib(address, lib_index) => {
                    let location_string = format!("0x{:x}", address);
                    let s = string_table.index_for_string(&location_string);
                    let res = resource_table.resource_for_lib(lib_index, global_libs, string_table);
                    (Some(address), s, Some(res))
                }
                InternalFrame::Label(string_index) => (None, string_index, None),
            };
            frame_addresses.push(address);
            func_names.push(location_string_index);
            func_resources.push(resource);
            frame_index
        })
    }

    pub fn as_frame_table(&self) -> SerializableFrameTable<'_> {
        SerializableFrameTable(self)
    }

    pub fn as_func_table(&self) -> SerializableFuncTable<'_> {
        SerializableFuncTable(self)
    }
}

struct SerializableFrameTable<'a>(&'a FrameTableAndFuncTable);

impl<'a> Serialize for SerializableFrameTable<'a> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let len = self.0.frame_addresses.len();
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("length", &len)?;
        map.serialize_entry("address", &SerializableFrameTableAddressColumn(self.0))?;
        map.serialize_entry("inlineDepth", &SerializableSingleValueColumn(0, len))?;
        map.serialize_entry("category", &SerializableSingleValueColumn(0, len))?;
        map.serialize_entry("subcategory", &SerializableSingleValueColumn(0, len))?;
        map.serialize_entry("func", &SerializableRange(len))?;
        map.serialize_entry("nativeSymbol", &SerializableSingleValueColumn((), len))?;
        map.serialize_entry("innerWindowID", &SerializableSingleValueColumn((), len))?;
        map.serialize_entry("implementation", &SerializableSingleValueColumn((), len))?;
        map.serialize_entry("line", &SerializableSingleValueColumn((), len))?;
        map.serialize_entry("column", &SerializableSingleValueColumn((), len))?;
        map.serialize_entry("optimizations", &SerializableSingleValueColumn((), len))?;
        map.end()
    }
}

struct SerializableFuncTable<'a>(&'a FrameTableAndFuncTable);

impl<'a> Serialize for SerializableFuncTable<'a> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let len = self.0.frame_addresses.len();
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("length", &len)?;
        map.serialize_entry("name", &self.0.func_names)?;
        map.serialize_entry("isJS", &SerializableSingleValueColumn(false, len))?;
        map.serialize_entry("relevantForJS", &SerializableSingleValueColumn(false, len))?;
        map.serialize_entry("resource", &SerializableFuncTableResourceColumn(self.0))?;
        map.serialize_entry("fileName", &SerializableSingleValueColumn((), len))?;
        map.serialize_entry("lineNumber", &SerializableSingleValueColumn((), len))?;
        map.serialize_entry("columnNumber", &SerializableSingleValueColumn((), len))?;
        map.end()
    }
}

impl Serialize for ThreadInternalStringIndex {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u32(self.0 .0)
    }
}

impl Serialize for GlobalLibIndex {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u32(self.0 as u32)
    }
}

impl Serialize for ResourceIndex {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u32(self.0 as u32)
    }
}

struct SerializableRange(usize);

impl Serialize for SerializableRange {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(0..self.0)
    }
}

struct SerializableSingleValueColumn<T: Serialize>(T, usize);

impl<T: Serialize> Serialize for SerializableSingleValueColumn<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.1))?;
        for _ in 0..self.1 {
            seq.serialize_element(&self.0)?;
        }
        seq.end()
    }
}

struct SerializableFrameTableAddressColumn<'a>(&'a FrameTableAndFuncTable);

impl<'a> Serialize for SerializableFrameTableAddressColumn<'a> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.0.frame_addresses.len()))?;
        for address in &self.0.frame_addresses {
            match address {
                Some(address) => seq.serialize_element(&address)?,
                None => seq.serialize_element(&-1)?,
            }
        }
        seq.end()
    }
}

struct SerializableFuncTableResourceColumn<'a>(&'a FrameTableAndFuncTable);

impl<'a> Serialize for SerializableFuncTableResourceColumn<'a> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.0.func_resources.len()))?;
        for resource in &self.0.func_resources {
            match resource {
                Some(resource) => seq.serialize_element(&resource)?,
                None => seq.serialize_element(&-1)?,
            }
        }
        seq.end()
    }
}

#[derive(Debug, Clone, Default)]
struct ResourceTable {
    resource_libs: Vec<GlobalLibIndex>,
    resource_names: Vec<ThreadInternalStringIndex>,
    lib_to_resource: FastHashMap<GlobalLibIndex, ResourceIndex>,
}

#[derive(Debug, Clone, Copy, PartialOrd, Ord, PartialEq, Eq, Hash)]
struct ResourceIndex(u32);

impl ResourceTable {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn resource_for_lib(
        &mut self,
        lib_index: GlobalLibIndex,
        global_libs: &GlobalLibTable,
        thread_string_table: &mut ThreadStringTable,
    ) -> ResourceIndex {
        let resource_libs = &mut self.resource_libs;
        let resource_names = &mut self.resource_names;
        *self.lib_to_resource.entry(lib_index).or_insert_with(|| {
            let resource = ResourceIndex(resource_libs.len() as u32);
            let lib_name = global_libs.lib_name(lib_index);
            resource_libs.push(lib_index);
            resource_names.push(thread_string_table.index_for_string(lib_name));
            resource
        })
    }
}

impl Serialize for ResourceTable {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        const RESOURCE_TYPE_LIB: u32 = 1;
        let len = self.resource_libs.len();
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("length", &len)?;
        map.serialize_entry("lib", &self.resource_libs)?;
        map.serialize_entry("name", &self.resource_names)?;
        map.serialize_entry("host", &SerializableSingleValueColumn((), len))?;
        map.serialize_entry(
            "type",
            &SerializableSingleValueColumn(RESOURCE_TYPE_LIB, len),
        )?;
        map.end()
    }
}

#[derive(Debug, Clone, Default)]
struct SampleTable {
    sample_weights: Vec<i32>,
    sample_timestamps: Vec<Timestamp>,
    sample_stack_indexes: Vec<Option<usize>>,
    sample_cpu_deltas: Vec<CpuDelta>,
}

impl SampleTable {
    pub fn new() -> Self {
        Default::default()
    }
}

impl Serialize for SampleTable {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let len = self.sample_timestamps.len();
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("length", &len)?;
        map.serialize_entry("stack", &self.sample_stack_indexes)?;
        map.serialize_entry("time", &self.sample_timestamps)?;
        map.serialize_entry("weight", &self.sample_weights)?;
        map.serialize_entry("weightType", &"samples")?;
        map.serialize_entry("threadCPUDelta", &self.sample_cpu_deltas)?;
        map.end()
    }
}

#[derive(Debug, Clone, Default)]
struct MarkerTable {
    marker_name_string_indexes: Vec<ThreadInternalStringIndex>,
    marker_starts: Vec<Option<Timestamp>>,
    marker_ends: Vec<Option<Timestamp>>,
    marker_phases: Vec<Phase>,
    marker_datas: Vec<Value>,
}

impl MarkerTable {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn add_marker(
        &mut self,
        name: ThreadInternalStringIndex,
        timing: MarkerTiming,
        data: Value,
    ) {
        let (s, e, phase) = match timing {
            MarkerTiming::Instant(s) => (Some(s), None, Phase::Instant),
            MarkerTiming::Interval(s, e) => (Some(s), Some(e), Phase::Interval),
            MarkerTiming::IntervalStart(s) => (Some(s), None, Phase::IntervalStart),
            MarkerTiming::IntervalEnd(e) => (None, Some(e), Phase::IntervalEnd),
        };
        self.marker_name_string_indexes.push(name);
        self.marker_starts.push(s);
        self.marker_ends.push(e);
        self.marker_phases.push(phase);
        self.marker_datas.push(data);
    }
}

impl Serialize for MarkerTable {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let len = self.marker_name_string_indexes.len();
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("length", &len)?;
        map.serialize_entry("category", &SerializableSingleValueColumn(0, len))?;
        map.serialize_entry("data", &self.marker_datas)?;
        map.serialize_entry(
            "endTime",
            &SerializableOptionalTimestampColumn(&self.marker_ends),
        )?;
        map.serialize_entry("name", &self.marker_name_string_indexes)?;
        map.serialize_entry("phase", &self.marker_phases)?;
        map.serialize_entry(
            "startTime",
            &SerializableOptionalTimestampColumn(&self.marker_starts),
        )?;
        map.end()
    }
}
struct SerializableOptionalTimestampColumn<'a>(&'a [Option<Timestamp>]);

impl<'a> Serialize for SerializableOptionalTimestampColumn<'a> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for timestamp in self.0 {
            match timestamp {
                Some(timestamp) => seq.serialize_element(&timestamp)?,
                None => seq.serialize_element(&0.0)?,
            }
        }
        seq.end()
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
enum Phase {
    Instant = 0,
    Interval = 1,
    IntervalStart = 2,
    IntervalEnd = 3,
}

impl Serialize for Phase {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(*self as u8)
    }
}

#[derive(Debug, Clone, Default)]
struct StringTable {
    strings: Vec<String>,
    index: FastHashMap<String, StringIndex>,
}

impl StringTable {
    pub fn index_for_string(&mut self, s: &str) -> StringIndex {
        match self.index.get(s) {
            Some(string_index) => *string_index,
            None => {
                let string_index = StringIndex(self.strings.len() as u32);
                self.strings.push(s.to_string());
                self.index.insert(s.to_string(), string_index);
                string_index
            }
        }
    }

    pub fn get_string(&self, index: StringIndex) -> Option<&str> {
        self.strings.get(index.0 as usize).map(Deref::deref)
    }
}

#[derive(Debug, Clone, Default)]
struct ThreadStringTable {
    table: StringTable,
    global_to_local_string: FastHashMap<GlobalStringIndex, ThreadInternalStringIndex>,
}

impl ThreadStringTable {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn index_for_string(&mut self, s: &str) -> ThreadInternalStringIndex {
        ThreadInternalStringIndex(self.table.index_for_string(s))
    }

    pub fn index_for_global_string(
        &mut self,
        global_index: GlobalStringIndex,
        global_table: &GlobalStringTable,
    ) -> ThreadInternalStringIndex {
        let table = &mut self.table;
        *self
            .global_to_local_string
            .entry(global_index)
            .or_insert_with(|| {
                let s = global_table.get_string(global_index).unwrap();
                ThreadInternalStringIndex(table.index_for_string(s))
            })
    }
}

impl Serialize for ThreadStringTable {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.table.strings.serialize(serializer)
    }
}

#[derive(Debug, Clone, Default)]
struct GlobalStringTable {
    table: StringTable,
}

impl GlobalStringTable {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn index_for_string(&mut self, s: &str) -> GlobalStringIndex {
        GlobalStringIndex(self.table.index_for_string(s))
    }

    pub fn get_string(&self, index: GlobalStringIndex) -> Option<&str> {
        self.table.get_string(index.0)
    }
}

#[cfg(test)]
mod test {
    use assert_json_diff::assert_json_eq;
    use debugid::{CodeId, DebugId};
    use serde_json::json;
    use std::{str::FromStr, time::Duration};

    use crate::{
        CpuDelta, Frame, LibraryInfo, MarkerDynamicField, MarkerFieldFormat, MarkerLocation,
        MarkerSchema, MarkerSchemaField, MarkerStaticField, MarkerTiming, Profile, ProfilerMarker,
        ReferenceTimestamp, TextMarker, Timestamp,
    };

    #[test]
    fn it_works() {
        struct CustomMarker {
            event_name: String,
            allocation_size: u32,
            url: String,
            latency: Duration,
        }
        impl ProfilerMarker for CustomMarker {
            const MARKER_TYPE_NAME: &'static str = "custom";

            fn schema() -> MarkerSchema {
                MarkerSchema {
                    type_name: Self::MARKER_TYPE_NAME,
                    locations: vec![MarkerLocation::MarkerChart, MarkerLocation::MarkerTable],
                    chart_label: None,
                    tooltip_label: Some("Custom tooltip label"),
                    table_label: None,
                    fields: vec![
                        MarkerSchemaField::Dynamic(MarkerDynamicField {
                            key: "eventName",
                            label: "Event name",
                            format: MarkerFieldFormat::String,
                            searchable: None,
                        }),
                        MarkerSchemaField::Dynamic(MarkerDynamicField {
                            key: "allocationSize",
                            label: "Allocation size",
                            format: MarkerFieldFormat::Bytes,
                            searchable: None,
                        }),
                        MarkerSchemaField::Dynamic(MarkerDynamicField {
                            key: "url",
                            label: "URL",
                            format: MarkerFieldFormat::Url,
                            searchable: None,
                        }),
                        MarkerSchemaField::Dynamic(MarkerDynamicField {
                            key: "latency",
                            label: "Latency",
                            format: MarkerFieldFormat::Duration,
                            searchable: None,
                        }),
                        MarkerSchemaField::Static(MarkerStaticField {
                            label: "Description",
                            value: "This is a test marker with a custom schema.",
                        }),
                    ],
                }
            }

            fn json_marker_data(&self) -> serde_json::Value {
                json!({
                    "type": Self::MARKER_TYPE_NAME,
                    "eventName": self.event_name,
                    "allocationSize": self.allocation_size,
                    "url": self.url,
                    "latency": self.latency.as_secs_f64() * 1000.0,
                })
            }
        }

        let mut profile = Profile::new(
            "test",
            ReferenceTimestamp::from_millis_since_unix_epoch(1636162232627.0),
            Duration::from_millis(1),
        );
        let process = profile.add_process("test", 123, Timestamp::from_millis_since_reference(0.0));
        let thread = profile.add_thread(
            process,
            12345,
            Timestamp::from_millis_since_reference(0.0),
            true,
        );

        profile.add_sample(
            thread,
            Timestamp::from_millis_since_reference(0.0),
            vec![].into_iter(),
            CpuDelta::ZERO,
            1,
        );
        profile.add_lib(
            process,
            LibraryInfo {
                name: "libc.so.6".to_string(),
                debug_name: "libc.so.6".to_string(),
                path: "/usr/lib/x86_64-linux-gnu/libc.so.6".to_string(),
                code_id: Some(
                    CodeId::from_str("f0fc29165cbe6088c0e1adf03b0048fbecbc003a").unwrap(),
                ),
                debug_path: "/usr/lib/x86_64-linux-gnu/libc.so.6".to_string(),
                debug_id: DebugId::from_breakpad("1629FCF0BE5C8860C0E1ADF03B0048FB0").unwrap(),
                arch: None,
                base_avma: 0x00007f76b7e5d000,
                avma_range: 0x00007f76b7e85000..0x00007f76b8019000,
            },
        );
        profile.add_lib(
            process,
            LibraryInfo {
                name: "dump_syms".to_string(),
                debug_name: "dump_syms".to_string(),
                path: "/home/mstange/code/dump_syms/target/release/dump_syms".to_string(),
                code_id: Some(
                    CodeId::from_str("510d0a5c19eadf8043f203b4525be9be3dcb9554").unwrap(),
                ),
                debug_path: "/home/mstange/code/dump_syms/target/release/dump_syms".to_string(),
                debug_id: DebugId::from_breakpad("5C0A0D51EA1980DF43F203B4525BE9BE0").unwrap(),
                arch: None,
                base_avma: 0x000055ba9eb4d000,
                avma_range: 0x000055ba9ebf6000..0x000055ba9f07e000,
            },
        );
        profile.add_sample(
            thread,
            Timestamp::from_millis_since_reference(1.0),
            vec![
                0x7f76b7ffc0e7,
                0x55ba9eda3d7f,
                0x55ba9ed8bb62,
                0x55ba9ec92419,
                0x55ba9ec2b778,
                0x55ba9ec0f705,
                0x7ffdb4824838,
            ]
            .into_iter()
            .enumerate()
            .rev()
            .map(|(i, addr)| {
                if i == 0 {
                    Frame::InstructionPointer(addr)
                } else {
                    Frame::ReturnAddress(addr)
                }
            }),
            CpuDelta::ZERO,
            1,
        );
        profile.add_sample(
            thread,
            Timestamp::from_millis_since_reference(2.0),
            vec![
                0x55ba9eda018e,
                0x55ba9ec3c3cf,
                0x55ba9ec2a2d7,
                0x55ba9ec53993,
                0x7f76b7e8707d,
                0x55ba9ec0f705,
                0x7ffdb4824838,
            ]
            .into_iter()
            .enumerate()
            .rev()
            .map(|(i, addr)| {
                if i == 0 {
                    Frame::InstructionPointer(addr)
                } else {
                    Frame::ReturnAddress(addr)
                }
            }),
            CpuDelta::ZERO,
            1,
        );
        profile.add_sample(
            thread,
            Timestamp::from_millis_since_reference(3.0),
            vec![
                0x7f76b7f019c6,
                0x55ba9edc48f5,
                0x55ba9ec010e3,
                0x55ba9eca41b9,
                0x7f76b7e8707d,
                0x55ba9ec0f705,
                0x7ffdb4824838,
            ]
            .into_iter()
            .enumerate()
            .rev()
            .map(|(i, addr)| {
                if i == 0 {
                    Frame::InstructionPointer(addr)
                } else {
                    Frame::ReturnAddress(addr)
                }
            }),
            CpuDelta::ZERO,
            1,
        );
        profile.add_marker(
            thread,
            "Experimental",
            TextMarker("Hello world!".to_string()),
            MarkerTiming::Instant(Timestamp::from_millis_since_reference(0.0)),
        );
        profile.add_marker(
            thread,
            "CustomName",
            CustomMarker {
                event_name: "My event".to_string(),
                allocation_size: 512000,
                url: "https://mozilla.org/".to_string(),
                latency: Duration::from_millis(123),
            },
            MarkerTiming::Interval(
                Timestamp::from_millis_since_reference(0.0),
                Timestamp::from_millis_since_reference(2.0),
            ),
        );
        // eprintln!("{:#?}", profile);
        eprintln!("{}", serde_json::to_string_pretty(&profile).unwrap());
        assert_json_eq!(
            profile,
            json!(
                {
                    "meta": {
                      "categories": [
                        {
                          "color": "blue",
                          "name": "Regular",
                          "subcategories": [
                            "Other"
                          ]
                        },
                        {
                          "color": "grey",
                          "name": "Other",
                          "subcategories": [
                            "Other"
                          ]
                        }
                      ],
                      "debug": false,
                      "extensions": {
                        "baseURL": [],
                        "id": [],
                        "length": 0,
                        "name": []
                      },
                      "interval": 1.0,
                      "markerSchema": [
                        {
                          "chartLabel": "{marker.data.name}",
                          "data": [
                            {
                              "format": "string",
                              "key": "name",
                              "label": "Details"
                            }
                          ],
                          "display": [
                            "marker-chart",
                            "marker-table"
                          ],
                          "name": "Text",
                          "tableLabel": "{marker.name} - {marker.data.name}"
                        },
                        {
                          "data": [
                            {
                              "format": "string",
                              "key": "eventName",
                              "label": "Event name"
                            },
                            {
                              "format": "bytes",
                              "key": "allocationSize",
                              "label": "Allocation size"
                            },
                            {
                              "format": "url",
                              "key": "url",
                              "label": "URL"
                            },
                            {
                              "format": "duration",
                              "key": "latency",
                              "label": "Latency"
                            },
                            {
                              "label": "Description",
                              "value": "This is a test marker with a custom schema."
                            }
                          ],
                          "display": [
                            "marker-chart",
                            "marker-table"
                          ],
                          "name": "custom",
                          "tooltipLabel": "Custom tooltip label"
                        }
                      ],
                      "pausedRanges": [],
                      "preprocessedProfileVersion": 41,
                      "processType": 0,
                      "product": "test",
                      "sampleUnits": {
                        "eventDelay": "ms",
                        "threadCPUDelta": "µs",
                        "time": "ms"
                      },
                      "startTime": 1636162232627.0,
                      "symbolicated": false,
                      "version": 24
                    },
                    "libs": [
                      {
                        "name": "dump_syms",
                        "path": "/home/mstange/code/dump_syms/target/release/dump_syms",
                        "debugName": "dump_syms",
                        "debugPath": "/home/mstange/code/dump_syms/target/release/dump_syms",
                        "breakpadId": "5C0A0D51EA1980DF43F203B4525BE9BE0",
                        "codeId": "510d0a5c19eadf8043f203b4525be9be3dcb9554",
                        "arch": null
                      },
                      {
                        "name": "libc.so.6",
                        "path": "/usr/lib/x86_64-linux-gnu/libc.so.6",
                        "debugName": "libc.so.6",
                        "debugPath": "/usr/lib/x86_64-linux-gnu/libc.so.6",
                        "breakpadId": "1629FCF0BE5C8860C0E1ADF03B0048FB0",
                        "codeId": "f0fc29165cbe6088c0e1adf03b0048fbecbc003a",
                        "arch": null
                      }
                    ],
                    "threads": [
                      {
                        "frameTable": {
                          "length": 16,
                          "address": [
                            -1,
                            796420,
                            911223,
                            1332248,
                            2354017,
                            2452862,
                            1700071,
                            172156,
                            1075602,
                            905942,
                            979918,
                            2437518,
                            1405368,
                            737506,
                            2586868,
                            674246
                          ],
                          "inlineDepth": [
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0
                          ],
                          "category": [
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0
                          ],
                          "subcategory": [
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0
                          ],
                          "func": [
                            0,
                            1,
                            2,
                            3,
                            4,
                            5,
                            6,
                            7,
                            8,
                            9,
                            10,
                            11,
                            12,
                            13,
                            14,
                            15
                          ],
                          "nativeSymbol": [
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null
                          ],
                          "innerWindowID": [
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null
                          ],
                          "implementation": [
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null
                          ],
                          "line": [
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null
                          ],
                          "column": [
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null
                          ],
                          "optimizations": [
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null
                          ]
                        },
                        "funcTable": {
                          "length": 16,
                          "name": [
                            0,
                            1,
                            3,
                            4,
                            5,
                            6,
                            7,
                            9,
                            10,
                            11,
                            12,
                            13,
                            14,
                            15,
                            16,
                            17
                          ],
                          "isJS": [
                            false,
                            false,
                            false,
                            false,
                            false,
                            false,
                            false,
                            false,
                            false,
                            false,
                            false,
                            false,
                            false,
                            false,
                            false,
                            false
                          ],
                          "relevantForJS": [
                            false,
                            false,
                            false,
                            false,
                            false,
                            false,
                            false,
                            false,
                            false,
                            false,
                            false,
                            false,
                            false,
                            false,
                            false,
                            false
                          ],
                          "resource": [
                            -1,
                            0,
                            0,
                            0,
                            0,
                            0,
                            1,
                            1,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            1
                          ],
                          "fileName": [
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null
                          ],
                          "lineNumber": [
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null
                          ],
                          "columnNumber": [
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null,
                            null
                          ]
                        },
                        "markers": {
                          "length": 2,
                          "category": [
                            0,
                            0
                          ],
                          "data": [
                            {
                              "name": "Hello world!",
                              "type": "Text"
                            },
                            {
                              "allocationSize": 512000,
                              "eventName": "My event",
                              "latency": 123.0,
                              "type": "custom",
                              "url": "https://mozilla.org/"
                            }
                          ],
                          "endTime": [
                            0.0,
                            2.0
                          ],
                          "name": [
                            18,
                            19
                          ],
                          "phase": [
                            0,
                            1
                          ],
                          "startTime": [
                            0.0,
                            0.0
                          ]
                        },
                        "name": "GeckoMain",
                        "nativeSymbols": {
                          "address": [],
                          "length": 0,
                          "libIndex": [],
                          "name": []
                        },
                        "pausedRanges": [],
                        "pid": 123,
                        "processName": "test",
                        "processShutdownTime": null,
                        "processStartupTime": 0.0,
                        "processType": "default",
                        "registerTime": 0.0,
                        "resourceTable": {
                          "length": 2,
                          "lib": [
                            0,
                            1
                          ],
                          "name": [
                            2,
                            8
                          ],
                          "host": [
                            null,
                            null
                          ],
                          "type": [
                            1,
                            1
                          ]
                        },
                        "samples": {
                          "length": 4,
                          "stack": [
                            null,
                            6,
                            11,
                            15
                          ],
                          "time": [
                            0.0,
                            1.0,
                            2.0,
                            3.0
                          ],
                          "weight": [
                            1,
                            1,
                            1,
                            1
                          ],
                          "weightType": "samples",
                          "threadCPUDelta": [
                            0,
                            0,
                            0,
                            0
                          ]
                        },
                        "stackTable": {
                          "length": 16,
                          "prefix": [
                            null,
                            0,
                            1,
                            2,
                            3,
                            4,
                            5,
                            1,
                            7,
                            8,
                            9,
                            10,
                            7,
                            12,
                            13,
                            14
                          ],
                          "frame": [
                            0,
                            1,
                            2,
                            3,
                            4,
                            5,
                            6,
                            7,
                            8,
                            9,
                            10,
                            11,
                            12,
                            13,
                            14,
                            15
                          ],
                          "category": [
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0
                          ],
                          "subcategory": [
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0
                          ]
                        },
                        "stringArray": [
                          "0x7ffdb4824837",
                          "0xc2704",
                          "dump_syms",
                          "0xde777",
                          "0x145418",
                          "0x23eb61",
                          "0x256d7e",
                          "0x19f0e7",
                          "libc.so.6",
                          "0x2a07c",
                          "0x106992",
                          "0xdd2d6",
                          "0xef3ce",
                          "0x25318e",
                          "0x1571b8",
                          "0xb40e2",
                          "0x2778f4",
                          "0xa49c6",
                          "Experimental",
                          "CustomName"
                        ],
                        "tid": 12345,
                        "unregisterTime": null
                      }
                    ],
                    "pages": [],
                    "profilerOverhead": [],
                    "counters": []
                  }
            )
        )
    }
}