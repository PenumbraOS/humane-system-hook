use std::pin::Pin;

use prost_types::Timestamp;
use tokio_stream::Stream;
use tonic::{Request, Response, Status};
use tracing::info;

use crate::db::{ContactRecord, Database};
use crate::proto::contacts::contacts_rpc_service_server::ContactsRpcService;
use crate::proto::contacts::*;

pub struct ContactsRpcServiceImpl {
    pub db: Database,
}

#[tonic::async_trait]
impl ContactsRpcService for ContactsRpcServiceImpl {
    #[allow(deprecated)]
    async fn get_contacts(
        &self,
        request: Request<GetContactsRequest>,
    ) -> Result<Response<ContactList>, Status> {
        info!(">>> Contacts.GetContacts");
        let search_term = request.into_inner().search_term.unwrap_or_default();
        let mut contacts = self
            .db
            .list_contacts()
            .await
            .map_err(|e| Status::internal(format!("failed to list contacts: {e}")))?;

        if !search_term.trim().is_empty() {
            contacts.retain(|contact| contact_matches_search(contact, &search_term));
        }

        Ok(Response::new(ContactList {
            contacts: contacts.into_iter().map(contact_to_proto).collect(),
            encrypted_contacts: vec![],
            encrypted_contacts_versions: vec![],
        }))
    }

    type GetContactsPaginatedStreamingStream =
        Pin<Box<dyn Stream<Item = Result<GetContactsStreamingPageResponse, Status>> + Send>>;

    async fn get_contacts_paginated_streaming(
        &self,
        request: Request<GetContactsStreamingPageRequest>,
    ) -> Result<Response<Self::GetContactsPaginatedStreamingStream>, Status> {
        info!(">>> Contacts.GetContactsPaginatedStreaming");
        let request = request.into_inner();
        let page_size = if request.page_size > 0 {
            request.page_size as usize
        } else {
            100
        };

        let mut responses = Vec::new();
        match request.streaming_request.and_then(|r| r.sync_option) {
            Some(get_contacts_streaming_request::SyncOption::LastSyncedTime(ts)) => {
                let since = timestamp_to_millis(&ts);
                let (contacts, deleted) =
                    self.db
                        .list_contact_changes_since(since)
                        .await
                        .map_err(|e| {
                            Status::internal(format!("failed to list contact changes: {e}"))
                        })?;

                responses.extend(contacts.into_iter().map(|contact| {
                    GetContactsStreamingResponse {
                        modified_time: Some(millis_to_timestamp(contact.modified_at)),
                        response: Some(get_contacts_streaming_response::Response::Contact(
                            contact_to_proto(contact),
                        )),
                    }
                }));
                responses.extend(deleted.into_iter().map(|(id, modified_at)| {
                    GetContactsStreamingResponse {
                        modified_time: Some(millis_to_timestamp(modified_at)),
                        response: Some(
                            get_contacts_streaming_response::Response::DeletedContactId(id),
                        ),
                    }
                }));
            }
            _ => {
                let contacts = self
                    .db
                    .list_contacts()
                    .await
                    .map_err(|e| Status::internal(format!("failed to list contacts: {e}")))?;
                let deleted = self.db.list_deleted_contacts().await.map_err(|e| {
                    Status::internal(format!("failed to list deleted contacts: {e}"))
                })?;
                responses.extend(contacts.into_iter().map(|contact| {
                    GetContactsStreamingResponse {
                        modified_time: Some(millis_to_timestamp(contact.modified_at)),
                        response: Some(get_contacts_streaming_response::Response::Contact(
                            contact_to_proto(contact),
                        )),
                    }
                }));
                responses.extend(deleted.into_iter().map(|(id, modified_at)| {
                    GetContactsStreamingResponse {
                        modified_time: Some(millis_to_timestamp(modified_at)),
                        response: Some(
                            get_contacts_streaming_response::Response::DeletedContactId(id),
                        ),
                    }
                }));
            }
        }

        let total_pages = std::cmp::max(1, responses.len().div_ceil(page_size)) as i32;
        let pages: Vec<_> = if responses.is_empty() {
            vec![Ok(GetContactsStreamingPageResponse {
                page_content: vec![],
                page_num: 1,
                total_pages,
            })]
        } else {
            responses
                .chunks(page_size)
                .enumerate()
                .map(|(index, chunk)| {
                    Ok(GetContactsStreamingPageResponse {
                        page_content: chunk.to_vec(),
                        page_num: index as i32 + 1,
                        total_pages,
                    })
                })
                .collect()
        };

        Ok(Response::new(Box::pin(tokio_stream::iter(pages))))
    }
}

#[allow(deprecated)]
fn contact_to_proto(contact: ContactRecord) -> Contact {
    Contact {
        id: contact.id,
        version: 0,
        emails: contact
            .emails
            .into_iter()
            .map(|email| Email {
                value: email.value,
                r#type: email.r#type,
            })
            .collect(),
        name: Some(Name {
            first_name: contact.name.first_name,
            last_name: contact.name.last_name,
            nickname: contact.name.nickname,
            display_name: contact.name.display_name,
        }),
        contact_actions: vec![],
        social_handles: vec![],
        telephone_numbers: vec![],
        temporary: contact.temporary,
        last_used_at: None,
        trusted: contact.trusted,
        emergency: contact.emergency,
        phone_numbers: contact
            .phone_numbers
            .into_iter()
            .map(|phone| PhoneNumber {
                value: phone.value,
                r#type: phone.r#type,
            })
            .collect(),
        contact_source: contact.contact_source.map(|name| ContactSource { name }),
        organization: contact.organization.map(|name| Organization { name }),
        modified_at: Some(millis_to_timestamp(contact.modified_at)),
        internal_favorite: contact.internal_favorite,
    }
}

fn timestamp_to_millis(ts: &Timestamp) -> i64 {
    ts.seconds.saturating_mul(1000) + i64::from(ts.nanos / 1_000_000)
}

fn millis_to_timestamp(ms: i64) -> Timestamp {
    Timestamp {
        seconds: ms.div_euclid(1000),
        nanos: (ms.rem_euclid(1000) * 1_000_000) as i32,
    }
}

fn contact_matches_search(contact: &ContactRecord, search_term: &str) -> bool {
    let needle = search_term.trim().to_lowercase();
    if needle.is_empty() {
        return true;
    }

    contact.name.first_name.to_lowercase().contains(&needle)
        || contact.name.last_name.to_lowercase().contains(&needle)
        || contact.name.nickname.to_lowercase().contains(&needle)
        || contact.name.display_name.to_lowercase().contains(&needle)
        || contact
            .emails
            .iter()
            .any(|email| email.value.to_lowercase().contains(&needle))
        || contact
            .phone_numbers
            .iter()
            .any(|phone| phone.value.to_lowercase().contains(&needle))
}
