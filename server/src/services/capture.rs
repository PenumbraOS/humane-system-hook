//! CaptureService — handles photo/video memory creation, upload, and deletion.

use std::sync::Arc;

use prost::Message;
use tokio::sync::Mutex;
use tonic::{Request, Response, Status};
use tracing::{info, warn};

use crate::proto::capture::capture_service_server::CaptureService;
use crate::proto::capture::*;
use crate::proto::common::encryption as common;
use crate::storage::{Location, MediaStore};

pub struct CaptureServiceImpl {
    pub store: Arc<Mutex<MediaStore>>,
    /// Address the server is reachable at (e.g. "192.168.1.125:9090").
    pub server_addr: String,
}

// ─── helpers ────────────────────────────────────────────────────────

/// Try to decode a LocationEnvelope from the (bypassed) encrypted_location field.
/// With the encryption hook active, EncryptedData.data contains the raw serialized
/// LocationEnvelope proto bytes instead of ciphertext.
fn decode_location(encrypted: &Option<common::EncryptedData>) -> Option<Location> {
    let enc = encrypted.as_ref()?;
    let envelope = common::LocationEnvelope::decode(enc.data.as_ref()).ok()?;
    Some(Location {
        latitude: envelope.latitude,
        longitude: envelope.longitude,
        accuracy: if envelope.accuracy != 0.0 {
            Some(envelope.accuracy)
        } else {
            None
        },
        human_readable: if envelope.human_readable.is_empty() {
            None
        } else {
            Some(envelope.human_readable)
        },
        full_address: if envelope.full_address.is_empty() {
            None
        } else {
            Some(envelope.full_address)
        },
    })
}

/// Format a prost Timestamp as an ISO 8601-ish string.
fn fmt_timestamp(ts: &Option<prost_types::Timestamp>) -> String {
    match ts {
        Some(t) => {
            // Simple formatting: seconds since epoch
            let secs = t.seconds;
            // Try to produce a readable date via chrono-free approach
            format!("{secs}")
        }
        None => chrono_now(),
    }
}

/// Current time as epoch seconds string (no chrono dependency).
fn chrono_now() -> String {
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}", dur.as_secs())
}

/// Generate filenames for a capture burst based on memory type and counts.
fn generate_burst_files(
    memory_uuid: &str,
    burst_index: usize,
    num_files: usize,
    is_video: bool,
) -> CaptureBurst {
    let burst_uuid = uuid::Uuid::new_v4().to_string();
    let files: Vec<CaptureFile> = (0..num_files)
        .map(|file_idx| {
            let file_uuid = uuid::Uuid::new_v4().to_string();
            let ext = if is_video { "mp4" } else { "jpg" };
            let base = format!("{memory_uuid}_{burst_index}_{file_idx}");

            // The device reads these filenames from the CreateMemoryResponse and echoes
            // them back in UploadRequest RPCs.  Per the decompiled AssetUploadWorkerImpl:
            //   - Photos (JPG mode): only `secure_filename` (field 5) is used
            //   - Photos (YUV mode): only `secure_raw_data_filename` (field 6) is used
            //   - Videos: `secure_filename` (5) + `imu_data_filename` (8) + `video_timing_data_filename` (9)
            //   - `metadata_filename` (field 4) is never read by the upload code
            CaptureFile {
                id: 0,
                index: file_idx as i64,
                filename: String::new(),          // deprecated (field 3)
                metadata_filename: String::new(), // unused by device upload code (field 4)
                secure_filename: format!("{base}.{ext}"),
                secure_raw_data_filename: String::new(),
                uuid: file_uuid,
                imu_data_filename: if is_video {
                    format!("{base}_imu.bin")
                } else {
                    String::new()
                },
                video_timing_data_filename: if is_video {
                    format!("{base}_timing.bin")
                } else {
                    String::new()
                },
            }
        })
        .collect();

    CaptureBurst {
        id: 0,
        index: burst_index as i64,
        files,
        uuid: burst_uuid,
    }
}

/// Collect all filenames from a list of bursts.
fn collect_filenames(bursts: &[CaptureBurst]) -> Vec<String> {
    let mut names = Vec::new();
    for burst in bursts {
        for f in &burst.files {
            if !f.secure_filename.is_empty() {
                names.push(f.secure_filename.clone());
            }
            if !f.secure_raw_data_filename.is_empty() {
                names.push(f.secure_raw_data_filename.clone());
            }
            if !f.imu_data_filename.is_empty() {
                names.push(f.imu_data_filename.clone());
            }
            if !f.video_timing_data_filename.is_empty() {
                names.push(f.video_timing_data_filename.clone());
            }
        }
    }
    names
}

// ─── trait impl ─────────────────────────────────────────────────────

#[tonic::async_trait]
impl CaptureService for CaptureServiceImpl {
    async fn declare_memory_create_intent(
        &self,
        request: Request<MemoryCreateIntentRequest>,
    ) -> Result<Response<MemoryCreateIntentResponse>, Status> {
        let req = request.into_inner();
        info!(
            device_local_id = req.device_local_id,
            memory_type = req.memory_type,
            ">>> Capture.DeclareMemoryCreateIntent"
        );
        Ok(Response::new(MemoryCreateIntentResponse {}))
    }

    async fn create_memory(
        &self,
        request: Request<CreateMemoryRequest>,
    ) -> Result<Response<CreateMemoryResponse>, Status> {
        let req = request.into_inner();
        let memory_uuid = uuid::Uuid::new_v4().to_string();

        info!(uuid = memory_uuid, ">>> Capture.CreateMemory");

        match req.request {
            Some(create_memory_request::Request::PhotoMemoryRequest(photo)) => {
                let num_bursts = photo.num_bursts.max(1) as usize;
                let num_per_burst = photo.num_pics_per_burst.max(1) as usize;

                // Decode location from bypassed encryption
                let location = decode_location(&photo.encrypted_location);
                if let Some(ref loc) = location {
                    info!(
                        lat = loc.latitude,
                        lon = loc.longitude,
                        "photo location decoded"
                    );
                }

                // Generate burst/file structure
                let bursts: Vec<CaptureBurst> = (0..num_bursts)
                    .map(|bi| generate_burst_files(&memory_uuid, bi, num_per_burst, false))
                    .collect();
                let filenames = collect_filenames(&bursts);
                let created_at = fmt_timestamp(&photo.device_created_time);

                // Create memory record + directory first (so thumbnail writes succeed)
                let mut store = self.store.lock().await;
                store
                    .create_memory(
                        memory_uuid.clone(),
                        "photo",
                        &photo.device_local_id,
                        &created_at,
                        filenames,
                        location,
                    )
                    .await
                    .map_err(|e| Status::internal(format!("storage error: {e}")))?;

                // Save thumbnails (plaintext JPEG bytes thanks to encryption bypass)
                for (i, thumb) in photo.thumbnails.iter().enumerate() {
                    if !thumb.data.is_empty() {
                        if let Err(e) = store.save_thumbnail(&memory_uuid, i, &thumb.data).await {
                            warn!(error = %e, "failed to save thumbnail {i}");
                        }
                    }
                }

                Ok(Response::new(CreateMemoryResponse {
                    status: CreateMemoryResultStatus::Success as i32,
                    memory: Some(Memory {
                        id: String::new(),
                        uuid: memory_uuid,
                    }),
                    response: Some(create_memory_response::Response::PhotoMemoryResponse(
                        PhotoMemoryResponse { bursts },
                    )),
                }))
            }

            Some(create_memory_request::Request::VideoMemoryRequest(video)) => {
                let num_videos = video.num_videos.max(1) as usize;
                let location = decode_location(&video.encrypted_location);
                if let Some(ref loc) = location {
                    info!(
                        lat = loc.latitude,
                        lon = loc.longitude,
                        "video location decoded"
                    );
                }

                let bursts = vec![generate_burst_files(&memory_uuid, 0, num_videos, true)];
                let filenames = collect_filenames(&bursts);
                let created_at = fmt_timestamp(&video.device_created_time);

                // Generate calibration data
                let calibration = CalibrationData {
                    device_should_upload: false,
                    filename: String::new(),
                };

                // Create memory record + directory first (so thumbnail write succeeds)
                let mut store = self.store.lock().await;
                store
                    .create_memory(
                        memory_uuid.clone(),
                        "video",
                        &video.device_local_id,
                        &created_at,
                        filenames,
                        location,
                    )
                    .await
                    .map_err(|e| Status::internal(format!("storage error: {e}")))?;

                // Save thumbnail
                if let Some(ref thumb) = video.thumbnail {
                    if !thumb.data.is_empty() {
                        if let Err(e) = store.save_thumbnail(&memory_uuid, 0, &thumb.data).await {
                            warn!(error = %e, "failed to save video thumbnail");
                        }
                    }
                }

                Ok(Response::new(CreateMemoryResponse {
                    status: CreateMemoryResultStatus::Success as i32,
                    memory: Some(Memory {
                        id: String::new(),
                        uuid: memory_uuid,
                    }),
                    response: Some(create_memory_response::Response::VideoMemoryResponse(
                        VideoMemoryResponse {
                            bursts,
                            calibration_data: Some(calibration),
                        },
                    )),
                }))
            }

            Some(create_memory_request::Request::FoodLogMemoryRequest(food)) => {
                let mut store = self.store.lock().await;
                let created_at = fmt_timestamp(&food.device_created_time);
                store
                    .create_memory(
                        memory_uuid.clone(),
                        "food_log",
                        &food.device_local_id,
                        &created_at,
                        vec![],
                        None,
                    )
                    .await
                    .map_err(|e| Status::internal(format!("storage error: {e}")))?;

                // If the food log data is present, save it
                if let Some(ref data) = food.food_log {
                    if !data.data.is_empty() {
                        if let Err(e) = store
                            .save_upload(&memory_uuid, "food_log.bin", &data.data)
                            .await
                        {
                            warn!(error = %e, "failed to save food log data");
                        }
                    }
                }

                Ok(Response::new(CreateMemoryResponse {
                    status: CreateMemoryResultStatus::Success as i32,
                    memory: Some(Memory {
                        id: String::new(),
                        uuid: memory_uuid,
                    }),
                    response: Some(create_memory_response::Response::FoodLogMemoryResponse(
                        FoodLogMemoryResponse {},
                    )),
                }))
            }

            Some(create_memory_request::Request::NoteMemoryRequest(note)) => {
                let mut store = self.store.lock().await;
                let location = decode_location(&note.encrypted_location);

                store
                    .create_memory(
                        memory_uuid.clone(),
                        "note",
                        "",
                        &chrono_now(),
                        vec![],
                        location,
                    )
                    .await
                    .map_err(|e| Status::internal(format!("storage error: {e}")))?;

                // Save note data
                if let Some(ref data) = note.encrypted_note {
                    if !data.data.is_empty() {
                        if let Err(e) = store
                            .save_upload(&memory_uuid, "note.bin", &data.data)
                            .await
                        {
                            warn!(error = %e, "failed to save note data");
                        }
                    }
                }

                Ok(Response::new(CreateMemoryResponse {
                    status: CreateMemoryResultStatus::Success as i32,
                    memory: Some(Memory {
                        id: String::new(),
                        uuid: memory_uuid,
                    }),
                    response: Some(create_memory_response::Response::NoteMemoryResponse(
                        NoteMemoryResponse {},
                    )),
                }))
            }

            None => Err(Status::invalid_argument("missing memory request oneof")),
        }
    }

    async fn upload_file(
        &self,
        request: Request<UploadRequest>,
    ) -> Result<Response<UploadResponse>, Status> {
        let req = request.into_inner();
        info!(
            filename = req.filename,
            upload_type = req.upload_type,
            mime = req.mime_encoding,
            ">>> Capture.UploadFile"
        );

        // Find which memory owns this filename
        let store = self.store.lock().await;
        let memory = store.find_memory_for_file(&req.filename);
        let uuid = match memory {
            Some(m) => m.uuid.clone(),
            None => {
                // The device might send filenames we didn't generate (e.g. calibration).
                // Use "unknown" bucket — still allow the upload.
                warn!(
                    filename = req.filename,
                    "upload for unknown file, using fallback"
                );
                "unknown".to_string()
            }
        };

        let url = format!(
            "http://{}/upload/{}/{}",
            self.server_addr, uuid, req.filename
        );

        Ok(Response::new(UploadResponse { url }))
    }

    async fn upload_complete(
        &self,
        request: Request<UploadCompleteRequest>,
    ) -> Result<Response<UploadCompleteResponse>, Status> {
        let req = request.into_inner();
        let uuid = &req.memory_uuid;
        let success = req.success;
        info!(
            uuid,
            success,
            retry = req.retry_number,
            ">>> Capture.UploadComplete"
        );

        let mut store = self.store.lock().await;
        if success == UploadCompletionStatus::UploadSuccess as i32 {
            match store.complete_memory(uuid).await {
                Ok(true) => info!(uuid, "memory marked complete"),
                Ok(false) => warn!(uuid, "memory not found for completion"),
                Err(e) => warn!(uuid, error = %e, "failed to complete memory"),
            }
            Ok(Response::new(UploadCompleteResponse {
                status: UploadCompleteStatus::ProcessingStarted as i32,
            }))
        } else {
            if let Err(e) = store.fail_memory(uuid).await {
                warn!(uuid, error = %e, "failed to mark memory as failed");
            }
            Ok(Response::new(UploadCompleteResponse {
                status: UploadCompleteStatus::Acknowledged as i32,
            }))
        }
    }

    async fn delete_memory(
        &self,
        request: Request<DeleteMemoryRequest>,
    ) -> Result<Response<DeleteMemoryResponse>, Status> {
        let req = request.into_inner();
        let uuid = &req.memory_uuid;
        info!(uuid, ">>> Capture.DeleteMemory");

        let mut store = self.store.lock().await;
        match store.delete_memory(uuid).await {
            Ok(true) => Ok(Response::new(DeleteMemoryResponse {
                status: DeleteMemoryStatus::Success as i32,
            })),
            Ok(false) => Ok(Response::new(DeleteMemoryResponse {
                status: DeleteMemoryStatus::NotFound as i32,
            })),
            Err(e) => {
                warn!(uuid, error = %e, "failed to delete memory");
                Ok(Response::new(DeleteMemoryResponse {
                    status: DeleteMemoryStatus::Failure as i32,
                }))
            }
        }
    }

    async fn get_capture_config(
        &self,
        _request: Request<GetCaptureConfigRequest>,
    ) -> Result<Response<GetCaptureConfigResponse>, Status> {
        info!(">>> Capture.GetCaptureConfig");
        Ok(Response::new(GetCaptureConfigResponse {
            num_photos_per_burst: 1,
            create_memory_retry_config: Some(CaptureRetryConfig {
                max_retries: 3,
                initial_delay_ms: 1000,
                backoff_multiplier: 2.0,
            }),
            asset_upload_retry_config: Some(CaptureRetryConfig {
                max_retries: 3,
                initial_delay_ms: 1000,
                backoff_multiplier: 2.0,
            }),
            delete_memory_retry_config: Some(CaptureRetryConfig {
                max_retries: 3,
                initial_delay_ms: 1000,
                backoff_multiplier: 2.0,
            }),
        }))
    }

    async fn report_photography_experience_status(
        &self,
        request: Request<ReportPhotographyExperienceStatusRequest>,
    ) -> Result<Response<ReportPhotographyExperienceStatusResponse>, Status> {
        let req = request.into_inner();
        info!(
            status = req.status,
            message = req.custom_message,
            ">>> Capture.ReportPhotographyExperienceStatus"
        );
        Ok(Response::new(ReportPhotographyExperienceStatusResponse {}))
    }

    async fn get_food_log_summary(
        &self,
        _request: Request<GetFoodLogSummaryRequest>,
    ) -> Result<Response<GetFoodLogSummaryResponse>, Status> {
        info!(">>> Capture.GetFoodLogSummary");
        Ok(Response::new(GetFoodLogSummaryResponse {
            food_log_summary: None,
        }))
    }

    async fn get_memory_share_link(
        &self,
        request: Request<GetShareLinkRequest>,
    ) -> Result<Response<GetShareLinkResponse>, Status> {
        let req = request.into_inner();
        info!(
            uuid = req.memory_uuid,
            ">>> Capture.GetMemoryShareLink (stub)"
        );
        Ok(Response::new(GetShareLinkResponse {
            share_link: String::new(),
        }))
    }

    async fn save_shared_memory(
        &self,
        _request: Request<SaveSharedMemoryRequest>,
    ) -> Result<Response<SaveSharedMemoryResponse>, Status> {
        info!(">>> Capture.SaveSharedMemory (stub)");
        Ok(Response::new(SaveSharedMemoryResponse {
            created_memory_uuid: String::new(),
        }))
    }

    async fn get_share_link_contents(
        &self,
        _request: Request<GetShareLinkContentsRequest>,
    ) -> Result<Response<GetShareLinkContentsResponse>, Status> {
        info!(">>> Capture.GetShareLinkContents (stub)");
        Ok(Response::new(GetShareLinkContentsResponse {
            decrypted_thumbnail_bytes: vec![],
        }))
    }
}
