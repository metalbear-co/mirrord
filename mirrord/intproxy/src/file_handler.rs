// use mirrord_protocol::{
//     file::{
//         AccessFileRequest, CloseDirRequest, CloseFileRequest, FdOpenDirRequest,
// GetDEnts64Request,         OpenFileRequest, OpenRelativeFileRequest, ReadDirRequest,
// ReadFileRequest,         ReadLimitedFileRequest, SeekFileRequest, WriteFileRequest,
// WriteLimitedFileRequest,         XstatFsRequest, XstatRequest,
//     },
//     ClientMessage, FileRequest, FileResponse,
// };

// use crate::{
//     agent_conn::AgentSender,
//     error::Result,
//     layer_conn::LayerSender,
//     request_queue::RequestQueue,
// };

// pub struct FileHandler {
//     agent_sender: AgentSender,

//     open_queue: RequestQueue<Open>,
//     read_queue: RequestQueue<Read>,
//     read_limited_queue: RequestQueue<ReadLimited>,
//     seek_queue: RequestQueue<Seek>,
//     write_queue: RequestQueue<Write>,
//     write_limited_queue: RequestQueue<WriteLimited>,
//     access_queue: RequestQueue<Access>,
//     xstat_queue: RequestQueue<Xstat>,
//     xstatfs_queue: RequestQueue<XstatFs>,
//     opendir_queue: RequestQueue<FdOpenDir>,
//     readdir_queue: RequestQueue<ReadDir>,
//     #[cfg(target_os = "linux")]
//     getdents64_queue: RequestQueue<GetDEnts64>,
// }

// impl FileHandler {
//     pub fn new(agent_sender: AgentSender, layer_sender: LayerSender) -> Self {
//         Self {
//             agent_sender,
//             open_queue: RequestQueue::new(layer_sender.clone()),
//             read_queue: RequestQueue::new(layer_sender.clone()),
//             read_limited_queue: RequestQueue::new(layer_sender.clone()),
//             seek_queue: RequestQueue::new(layer_sender.clone()),
//             write_queue: RequestQueue::new(layer_sender.clone()),
//             write_limited_queue: RequestQueue::new(layer_sender.clone()),
//             access_queue: RequestQueue::new(layer_sender.clone()),
//             xstat_queue: RequestQueue::new(layer_sender.clone()),
//             xstatfs_queue: RequestQueue::new(layer_sender.clone()),
//             opendir_queue: RequestQueue::new(layer_sender.clone()),
//             readdir_queue: RequestQueue::new(layer_sender.clone()),
//             #[cfg(target_os = "linux")]
//             getdents64_queue: RequestQueue::new(layer_sender),
//         }
//     }

//     #[tracing::instrument(level = "trace", skip(self))]
//     pub async fn handle_daemon_message(&mut self, message: FileResponse) -> Result<()> {
//         let res = match message {
//             FileResponse::Open(open) => self.open_queue.resolve_one(open).await,
//             FileResponse::Read(read) => self.read_queue.resolve_one(read).await,
//             FileResponse::ReadLimited(read) => self.read_limited_queue.resolve_one(read).await,
//             FileResponse::Seek(seek) => self.seek_queue.resolve_one(seek).await,
//             FileResponse::Write(write) => self.write_queue.resolve_one(write).await,
//             FileResponse::Access(access) => self.access_queue.resolve_one(access).await,
//             FileResponse::WriteLimited(write) =>
// self.write_limited_queue.resolve_one(write).await,             FileResponse::Xstat(xstat) =>
// self.xstat_queue.resolve_one(xstat).await,             FileResponse::XstatFs(xstatfs) =>
// self.xstatfs_queue.resolve_one(xstatfs).await,             FileResponse::ReadDir(read_dir) =>
// self.readdir_queue.resolve_one(read_dir).await,             FileResponse::OpenDir(open_dir) =>
// self.opendir_queue.resolve_one(open_dir).await,             #[cfg(target_os = "linux")]
//             FileResponse::GetDEnts64(getdents64) => {
//                 self.getdents64_queue.resolve_one(getdents64).await
//             }
//             #[cfg(not(target_os = "linux"))]
//             FileResponse::GetDEnts64(_) => {
//                 error!(
//                     "Received GetDEnts64Response on non-linux platform! Please report this to
// us!"                 );
//                 Ok(())
//             }
//         };

//         res.map_err(Into::into)
//     }

//     #[tracing::instrument(level = "trace", skip(self))]
//     pub async fn handle_hook_message(
//         &mut self,
//         request_id: u64,
//         request: FileOperation,
//     ) -> Result<()> { match request { FileOperation::Open(open) => self.handle_hook_open(open,
//       request_id).await, FileOperation::OpenRelative(open_relative) => {
//       self.handle_hook_open_relative(open_relative, request_id) .await }
//       FileOperation::Read(read) => self.handle_hook_read(read, request_id).await,
//       FileOperation::ReadLimited(read) => { self.handle_hook_read_limited(read, request_id).await
//       } FileOperation::Seek(seek) => self.handle_hook_seek(seek, request_id).await,
//       FileOperation::Write(write) => self.handle_hook_write(write, request_id).await,
//       FileOperation::WriteLimited(write) => { self.handle_hook_write_limited(write,
//       request_id).await } FileOperation::Close(close) => self.handle_hook_close(close,
//       request_id).await, FileOperation::Access(access) => self.handle_hook_access(access,
//       request_id).await, FileOperation::Xstat(xstat) => self.handle_hook_xstat(xstat,
//       request_id).await, FileOperation::XstatFs(xstatfs) => self.handle_hook_xstatfs(xstatfs,
//       request_id).await, FileOperation::ReadDir(read_dir) => {
//       self.handle_hook_read_dir(read_dir, request_id).await } FileOperation::FdOpenDir(open_dir)
//       => { self.handle_hook_fdopen_dir(open_dir, request_id).await }
//       FileOperation::CloseDir(close_dir) => { self.handle_hook_close_dir(close_dir,
//       request_id).await } #[cfg(target_os = "linux")] FileOperation::GetDEnts64(getdents64) => {
//       self.handle_hook_getdents64(getdents64, request_id).await } }
//     }

//     #[tracing::instrument(level = "trace", skip(self))]
//     async fn handle_hook_open(&mut self, open: Open, request_id: u64) -> Result<()> {
//         let Open { path, open_options } = open;

//         self.open_queue.queue(request_id);

//         let open_file_request = OpenFileRequest { path, open_options };
//         let request = ClientMessage::FileRequest(FileRequest::Open(open_file_request));

//         self.agent_sender.send(request).await.map_err(From::from)
//     }

//     #[tracing::instrument(level = "trace", skip(self))]
//     async fn handle_hook_open_relative(
//         &mut self,
//         open_relative: OpenRelative,
//         request_id: u64,
//     ) -> Result<()> { let OpenRelative { relative_fd, path, open_options, } = open_relative;

//         self.open_queue.queue(request_id);

//         let open_relative_file_request = OpenRelativeFileRequest {
//             relative_fd,
//             path,
//             open_options,
//         };
//         let request =
//             ClientMessage::FileRequest(FileRequest::OpenRelative(open_relative_file_request));

//         self.agent_sender.send(request).await.map_err(From::from)
//     }

//     #[tracing::instrument(level = "trace", skip(self))]
//     async fn handle_hook_read(&mut self, read: Read, request_id: u64) -> Result<()> {
//         let Read {
//             remote_fd,
//             buffer_size,
//         } = read;

//         self.read_queue.queue(request_id);

//         let read_file_request = ReadFileRequest {
//             remote_fd,
//             buffer_size,
//         };
//         let request = ClientMessage::FileRequest(FileRequest::Read(read_file_request));

//         self.agent_sender.send(request).await.map_err(From::from)
//     }

//     #[tracing::instrument(level = "trace", skip(self))]
//     async fn handle_hook_read_limited(&mut self, read: ReadLimited, request_id: u64) ->
// Result<()> {         let ReadLimited {
//             remote_fd,
//             buffer_size,
//             start_from,
//         } = read;

//         self.read_limited_queue.queue(request_id);

//         let read_file_request = ReadLimitedFileRequest {
//             remote_fd,
//             buffer_size,
//             start_from,
//         };
//         let request = ClientMessage::FileRequest(FileRequest::ReadLimited(read_file_request));

//         self.agent_sender.send(request).await.map_err(From::from)
//     }

//     #[tracing::instrument(level = "trace", skip(self))]
//     async fn handle_hook_seek(&mut self, seek: Seek, request_id: u64) -> Result<()> {
//         let Seek {
//             remote_fd: fd,
//             seek_from,
//         } = seek;

//         self.seek_queue.queue(request_id);

//         let seek_file_request = SeekFileRequest {
//             fd,
//             seek_from: seek_from.into(),
//         };
//         let request = ClientMessage::FileRequest(FileRequest::Seek(seek_file_request));

//         self.agent_sender.send(request).await.map_err(From::from)
//     }

//     #[tracing::instrument(level = "trace", skip(self))]
//     async fn handle_hook_write(&mut self, write: Write, request_id: u64) -> Result<()> {
//         let Write {
//             remote_fd: fd,
//             write_bytes,
//         } = write;

//         self.write_queue.queue(request_id);

//         let write_file_request = WriteFileRequest { fd, write_bytes };
//         let request = ClientMessage::FileRequest(FileRequest::Write(write_file_request));

//         self.agent_sender.send(request).await.map_err(From::from)
//     }

//     #[tracing::instrument(level = "trace", skip(self))]
//     async fn handle_hook_write_limited(
//         &mut self,
//         write: WriteLimited,
//         request_id: u64,
//     ) -> Result<()> { let WriteLimited { remote_fd, start_from, write_bytes, } = write;

//         self.write_limited_queue.queue(request_id);

//         let write_file_request = WriteLimitedFileRequest {
//             remote_fd,
//             start_from,
//             write_bytes,
//         };
//         let request = ClientMessage::FileRequest(FileRequest::WriteLimited(write_file_request));

//         self.agent_sender.send(request).await.map_err(From::from)
//     }

//     #[tracing::instrument(level = "trace", skip(self))]
//     async fn handle_hook_close(&mut self, close: Close, reuqest_id: u64) -> Result<()> {
//         let Close { fd } = close;

//         let close_file_request = CloseFileRequest { fd };
//         let request = ClientMessage::FileRequest(FileRequest::Close(close_file_request));

//         self.agent_sender.send(request).await.map_err(Into::into)
//     }

//     #[tracing::instrument(level = "trace", skip(self))]
//     async fn handle_hook_close_dir(&mut self, close: CloseDir, request_id: u64) -> Result<()> {
//         let CloseDir { fd } = close;

//         let close_dir_request = CloseDirRequest { remote_fd: fd };
//         let request = ClientMessage::FileRequest(FileRequest::CloseDir(close_dir_request));

//         self.agent_sender.send(request).await.map_err(From::from)
//     }

//     #[tracing::instrument(level = "trace", skip(self))]
//     async fn handle_hook_access(&mut self, access: Access, request_id: u64) -> Result<()> {
//         let Access {
//             path: pathname,
//             mode,
//         } = access;

//         self.access_queue.queue(request_id);

//         let access_file_request = AccessFileRequest { pathname, mode };
//         let request = ClientMessage::FileRequest(FileRequest::Access(access_file_request));

//         self.agent_sender.send(request).await.map_err(From::from)
//     }

//     #[tracing::instrument(level = "trace", skip(self))]
//     async fn handle_hook_xstat(&mut self, xstat: Xstat, request_id: u64) -> Result<()> {
//         let Xstat {
//             path,
//             fd,
//             follow_symlink,
//         } = xstat;

//         self.xstat_queue.queue(request_id);

//         let xstat_request = XstatRequest {
//             path,
//             fd,
//             follow_symlink,
//         };
//         let request = ClientMessage::FileRequest(FileRequest::Xstat(xstat_request));

//         self.agent_sender.send(request).await.map_err(From::from)
//     }

//     #[tracing::instrument(level = "trace", skip(self))]
//     async fn handle_hook_xstatfs(&mut self, xstat: XstatFs, request_id: u64) -> Result<()> {
//         let XstatFs { fd } = xstat;

//         self.xstatfs_queue.queue(request_id);

//         let xstatfs_request = XstatFsRequest { fd };
//         let request = ClientMessage::FileRequest(FileRequest::XstatFs(xstatfs_request));

//         self.agent_sender.send(request).await.map_err(From::from)
//     }

//     #[tracing::instrument(level = "trace", skip(self))]
//     async fn handle_hook_fdopen_dir(&mut self, open_dir: FdOpenDir, request_id: u64) ->
// Result<()> {         let FdOpenDir { remote_fd } = open_dir;

//         self.opendir_queue.queue(request_id);

//         let open_dir_request = FdOpenDirRequest { remote_fd };
//         let request = ClientMessage::FileRequest(FileRequest::FdOpenDir(open_dir_request));

//         self.agent_sender.send(request).await.map_err(From::from)
//     }

//     #[tracing::instrument(level = "trace", skip(self))]
//     async fn handle_hook_read_dir(&mut self, read_dir: ReadDir, request_id: u64) -> Result<()> {
//         let ReadDir { remote_fd } = read_dir;

//         self.readdir_queue.queue(request_id);

//         let read_dir_request = ReadDirRequest { remote_fd };
//         let request = ClientMessage::FileRequest(FileRequest::ReadDir(read_dir_request));

//         self.agent_sender.send(request).await.map_err(From::from)
//     }

//     #[cfg(target_os = "linux")]
//     #[tracing::instrument(level = "trace", skip(self))]
//     async fn handle_hook_getdents64(
//         &mut self,
//         getdents64: GetDEnts64,
//         request_id: u64,
//     ) -> Result<()> { let GetDEnts64 { remote_fd, buffer_size, } = getdents64;

//         self.getdents64_queue.queue(request_id);

//         let getdents_request = GetDEnts64Request {
//             remote_fd,
//             buffer_size,
//         };
//         let request = ClientMessage::FileRequest(FileRequest::GetDEnts64(getdents_request));

//         self.agent_sender.send(request).await.map_err(From::from)
//     }
// }
