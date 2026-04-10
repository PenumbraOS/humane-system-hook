# gRPC Service Reference

Complete inventory of all gRPC services on the Humane AI Pin, reconstructed from
decompiled ironman source. Covers service names, RPC methods, streaming types,
call patterns, gateway routing, error handling, and stubbing priorities.

Source: `/Users/adam/code/aipin/apk-environment/app/src/main/java/`

---

## Gateway Architecture

All gRPC traffic routes through `ChannelFactory`, which selects the endpoint based
on `GatewayType`. Three gateways exist:

| Gateway | Default Endpoint | TLS |
|---------|-----------------|-----|
| **API** | `api.prod.humane.cloud:443` | mTLS (port 443) or plaintext (other ports) |
| **ONBOARDING** | `onboarding.prod.humane.cloud:443` | mTLS |
| **E911GEO** | `e911geo.prod.humane.cloud:443` | mTLS |

The API gateway URI is read from `ChannelFactory.getGatewayUri()` — our hook
redirects this to the mock server. When the port is not 443, `ChannelFactory`
automatically uses `usePlaintext()` and adds an `X-Forwarded-Client-Cert` header
marked `"ForLocalDevelopment"` instead of real mTLS.

**13 services** use the API gateway. 1 uses ONBOARDING. 1 uses E911GEO.


## Authorization Model

Two interceptors (`AccountAuthorizationInterceptor`) wrap every gRPC call on the
API gateway. They check response trailers on error:

**De-authorization triggers (AVOID these in server responses):**
- `PERMISSION_DENIED` + trailer `unauthorized-device: <codes>` → device locked,
  keyguard engaged, user must re-provision
- `UNAUTHENTICATED` + trailer `subscription-status: <code>` → subscription revoked

**Safe error codes:**
- `UNIMPLEMENTED` — no auth check, no side effects (grpcio/tonic default)
- `UNAVAILABLE`, `DEADLINE_EXCEEDED`, `INTERNAL` — normal transient errors
- Any successful response (`OK`) → calls `resetAuthStateIfNeeded()` which
  **re-authorizes** the device if it was previously de-authorized

**Defaults (no server contact needed):**
- `isDeviceAuthorized()` → `true` (empty unauthorized list in SharedPreferences)
- `isSubscribed()` → `true` (status code 0 = UNSPECIFIED counts as subscribed)

**Storage:** SharedPreferences `"AccountAuthorizationStatus"` with keys
`"SubscriptionStatusCode"` (int) and `"UnauthorizedStatusCode"` (comma-delimited string).


## Connectivity Check

Two independent mechanisms determine "has internet":

### 1. Android Built-in (`hasInternet()`)

`NetworkMonitor.hasInternet()` checks `NET_CAPABILITY_INTERNET` (12) +
`NET_CAPABILITY_VALIDATED` (16) on the active network's `NetworkCapabilities`.

This is the **authoritative gate** used by:
- `RemoteInterpreter.interpret()` — blocks all AI queries when offline
- `IntentRecognitionAction` — plays different sound (connected vs. offline)
- `QuickActionTouchpadAction` — blocks messages/notes when offline
- `HybridSpeechSynthesizer` — falls back to local TTS when offline

Android validates by hitting `connectivitycheck.gstatic.com` — works on any
network with real internet access.

### 2. Humane Custom (`validateNetworkConnection()`)

`NetworkUtils.validateNetworkConnection(network)` makes an HTTP GET to
`http://connectivity-check.prod.humane.cloud`, expects HTTP 204.

- **204** → `CONNECTED`
- **301/302/303/305/307/308** → `WALLED_GARDEN` (captive portal)
- **Other / IOException** → `FAILURE`

Used **only** for WiFi UI status (captive portal warnings on home screen).
NOT used in the `hasInternet()` gate. Cosmetic, not functional.

**Source files:**
- `humaneinternal/system/network/NetworkUtils.java`
- `humaneinternal/system/network/NetworkMonitor.java`


---

## Active Services (15 services, 78 RPCs)

Services the device actually instantiates as gRPC client stubs.

### 1. AIBusService — AI Query Pipeline

| | |
|---|---|
| **Proto service** | `humane.aibus.AIBusService` |
| **Gateway** | API |
| **Source** | `humane/aibus/AIBusServiceGrpc.java` |
| **Stub created in** | `AIBusServiceFactory.java` → consumed by `AIBusService.java` |
| **Stub types** | async + blocking |
| **Trigger** | User action (voice query, image analysis, translation, etc.) |
| **Deadlines** | 25s (Understand), 15s (most encrypted), 5s (navigation) |

The primary AI service. 25 RPCs, but most are `Encrypted*` variants that use
Krypton application-layer encryption. Since `DISABLE_APP_ENCRYPTION = true`,
the unencrypted `Understand` RPC is the active code path.

| # | Method | Type | Notes |
|---|--------|------|-------|
| 1 | **Understand** | server-streaming | **PRIMARY** — voice queries. Extract utterance, return Respond action |
| 2 | UploadFile | unary | Training data upload |
| 3 | AnalyzeImage | unary | Vision/camera queries (unencrypted path) |
| 4 | FunctionExecution | unary | Tool/function call execution |
| 5 | ServerStatefulUnderstand | unary | Stateful conversation |
| 6 | BidirectionalStreamingUnderstand | bidi | Streaming conversation |
| 7 | Translate | unary | Translation (uses encrypted request type) |
| 8 | EncryptedStreamAIBus | bidi | Encrypted streaming AI |
| 9 | EncryptedUnderstand | server-streaming | Encrypted Understand (dead code) |
| 10 | EncryptedLoadingMessage | unary | Loading message fetch |
| 11 | EncryptedNearbySearch | unary | Location-based search |
| 12 | EncryptedNavigationDirections | unary | Turn-by-turn directions |
| 13 | EncryptedChatCompletion | unary | Chat completion |
| 14 | EncryptedCompletion | unary | Text completion |
| 15 | EncryptedGeoLocate | unary | Geolocation |
| 16 | EncryptedSmartPlaylist | unary | Music playlist generation |
| 17 | EncryptedWeather | unary | Weather queries |
| 18 | EncryptedReverseGeocode | unary | Reverse geocoding |
| 19 | EncryptedFunctionExecution | unary | Encrypted function calls |
| 20 | EncryptedAnalyzeImage | unary | Encrypted image analysis |
| 21 | EncryptedAnalyzeFoodImage | unary | Food image analysis |
| 22 | EncryptedGetFoodItem | unary | Food identification |
| 23 | EncryptedActionBasedInterstitial | unary | Action interstitials |
| 24 | ActionExecutionTest | unary | Test-only |
| 25 | TranscriptionRepairTest | unary | Test-only |


### 2. SpeechService — Text-to-Speech

| | |
|---|---|
| **Proto service** | `humane.aibus.SpeechService` |
| **Gateway** | API |
| **Source** | `humane/aibus/SpeechServiceGrpc.java` |
| **Stub created in** | `AIBusServiceFactory.java` → consumed by `AIBusService.java` |
| **Stub types** | async + blocking |
| **Trigger** | User action (TTS after AI response, translation) |
| **Deadlines** | Controlled by feature flag `SERVER_SPEECH_SYNTHESIS_TIMEOUT_MILLIS` |

`HybridSpeechSynthesizer` tries remote TTS first. On failure (including
`UNIMPLEMENTED`), falls back to local Android TTS gracefully.

| # | Method | Type | Notes |
|---|--------|------|-------|
| 1 | TextToSpeech | unary | Blocking call for speech synthesis |
| 2 | StreamingTextToSpeech | server-streaming | Streaming TTS (gated by feature flag) |
| 3 | CanTranslate | unary | Translation capability check |
| 4 | TranslateText | unary | Text translation |
| 5 | TranslateConversation | bidi | Live translation session |


### 3. CompositionService — Message Drafting & Notification Summary

| | |
|---|---|
| **Proto service** | `humane.aibus.CompositionService` |
| **Gateway** | API |
| **Source** | `humane/aibus/CompositionServiceGrpc.java` |
| **Stub created in** | `AiBrainService.java` (boot) |
| **Stub types** | async |
| **Trigger** | User action (message composition); automatic (incoming notifications) |

| # | Method | Type | Notes |
|---|--------|------|-------|
| 1 | EncryptedComposeMessage | unary | Encrypted message drafting |
| 2 | EncryptedSummarizeMessages | unary | Encrypted message summarization |
| 3 | CategorizeNotifications | unary | Notification categorization |
| 4 | SummarizeNotifications | unary | Notification summarization |


### 4. CaptureService — Photos, Videos, Food Logging

| | |
|---|---|
| **Proto service** | `humane.capture.CaptureService` |
| **Gateway** | API |
| **Source** | `humane/capture/CaptureServiceGrpc.java` |
| **Stub created in** | `system/CaptureService.java` (async, 10s), `ShareLinkUtil.java` (async, 15s), `FoodServiceWrapper` (future) |
| **Stub types** | async + future |
| **Trigger** | User action (photograph, video, share link, food logging) |
| **Deadlines** | 10s default, 15s share link |

| # | Method | Type | Notes |
|---|--------|------|-------|
| 1 | CreateMemory | unary | Create photo/video memory |
| 2 | DeleteMemory | unary | Delete memory |
| 3 | UploadComplete | unary | Signal upload finished |
| 4 | UploadFile | unary | Upload media file |
| 5 | ReportPhotographyExperienceStatus | unary | Photography status reporting |
| 6 | GetCaptureConfig | unary | Capture configuration |
| 7 | GetFoodLogSummary | unary | Daily food log summary |
| 8 | GetMemoryShareLink | unary | Generate share link |
| 9 | SaveSharedMemory | unary | Save from share link |
| 10 | GetShareLinkContents | unary | Fetch shared content |
| 11 | DeclareMemoryCreateIntent | unary | Pre-declare capture intent |


### 5. EventsIngestService — Telemetry Streaming

| | |
|---|---|
| **Proto service** | `humane.events.EventsIngestService` |
| **Gateway** | API |
| **Source** | `humane/events/EventsIngestServiceGrpc.java` |
| **Stub created in** | `AiBrainService.java` (boot) |
| **Stub types** | async with `withWaitForReady()` |
| **Trigger** | Boot — persistent bidi stream, events streamed continuously |

The stub uses `withWaitForReady()` so it queues events until the channel connects.
Non-fatal on failure — events are just lost.

| # | Method | Type | Notes |
|---|--------|------|-------|
| 1 | Ingest | bidi | Individual event streaming |
| 2 | IngestBatch | bidi | Batch event streaming |


### 6. DeviceEventsHistoryService — Event Sync

| | |
|---|---|
| **Proto service** | `humane.events.DeviceEventsHistoryService` |
| **Gateway** | API |
| **Source** | `humane/events/DeviceEventsHistoryServiceGrpc.java` |
| **Stub created in** | `AiBrainService.java` (boot) → `NotableEventsSyncManager` |
| **Stub types** | async |
| **Trigger** | Boot (sync), possibly periodic |

| # | Method | Type | Notes |
|---|--------|------|-------|
| 1 | QueryEvents | unary | Query historical events |


### 7. UserInformationService — User Profile

| | |
|---|---|
| **Proto service** | `humane.account.UserInformationService` |
| **Gateway** | API |
| **Source** | `humane/account/UserInformationServiceGrpc.java` |
| **Stub created in** | `AIBusServiceFactory.java` → consumed by `AIBusService.java` |
| **Stub types** | async |
| **Trigger** | Boot (one-time preferred name fetch, gated by SharedPreferences flag) |

| # | Method | Type | Notes |
|---|--------|------|-------|
| 1 | GetUserPersonalDetails | unary | Returns user's preferred name, used in AI prompts |


### 8. PublicPrivacyService — Krypton Key Management

| | |
|---|---|
| **Proto service** | `humane.privacy.grpc.pub.PublicPrivacyService` |
| **Gateway** | API |
| **Source** | `humane/privacy/grpc/pub/PublicPrivacyServiceGrpc.java` |
| **Stub created in** | `PrivacyClient.java` (lazy connect on first call) |
| **Stub types** | **blocking** |
| **Trigger** | On-demand by KryptoService (key operations, settings sync) |
| **Deadlines** | 5-minute auto-disconnect alarm after last call |

Since `DISABLE_APP_ENCRYPTION = true`, Krypton is effectively inactive. This
service is unlikely to be called in practice. If it is, `UNIMPLEMENTED` is safe.

| # | Method | Type | Notes |
|---|--------|------|-------|
| 1 | EstablishWrappingKeys | unary | Key wrapping setup |
| 2 | ImportKeys | unary | Key import |
| 3 | RequestKeys | unary | Key request |
| 4 | UpdateKeys | unary | Key update |
| 5 | RemoveKeys | unary | Key removal |
| 6 | SyncKeys | unary | Key synchronization |
| 7 | GetSettings | unary | Privacy settings fetch |
| 8 | UpdateSettings | unary | Privacy settings update |
| 9 | GetConfiguration | unary | Privacy configuration |


### 9. FeatureFlagsService — Feature Flag Sync

| | |
|---|---|
| **Proto service** | `humane.featureflags.FeatureFlagsService` |
| **Gateway** | API |
| **Source** | `humane/featureflags/FeatureFlagsServiceGrpc.java` |
| **Stub created in** | `FeatureFlagSyncWorker.java` |
| **Stub types** | async |
| **Trigger** | Periodic (daily via WorkManager); also at boot; also on push notification |

| # | Method | Type | Notes |
|---|--------|------|-------|
| 1 | GetFlags | unary | Returns feature flag assignments. Empty response = all defaults |


### 10. ContactsRPCService — Contacts Sync

| | |
|---|---|
| **Proto service** | `humane.contacts.ContactsRPCService` |
| **Gateway** | API |
| **Source** | `humane/contacts/ContactsRPCServiceGrpc.java` |
| **Stub created in** | `AppController.java` (boot) → `ContactsService` → `ContactsManager` |
| **Stub types** | async |
| **Trigger** | Boot (initial sync); push-triggered; user action (contact ops) |

| # | Method | Type | Notes |
|---|--------|------|-------|
| 1 | GetContacts | unary | Full contact list |
| 2 | GetContactDeltas | unary | Incremental sync |
| 3 | GetContactsStreaming | server-streaming | Streaming full fetch |
| 4 | GetContactsPaginatedStreaming | server-streaming | Paginated streaming fetch |
| 5 | CreateContacts | unary | Create contacts |
| 6 | UpdateContacts | unary | Update contacts |
| 7 | DeleteContacts | unary | Delete contacts |


### 11. PushRelayService — Push Notifications

| | |
|---|---|
| **Proto service** | `humane.pushrelay.PushRelayService` |
| **Gateway** | API |
| **Source** | `humane/pushrelay/PushRelayServiceGrpc.java` |
| **Stub created in** | `AppController.java` (boot) → `PushRelayGrpcClient`; `PushNetworkManager.java` |
| **Stub types** | async |
| **Trigger** | Boot — persistent bidi stream; recreated on WiFi/Cell network changes |
| **Retry** | `FuzzedExponentialBackoffPolicy` for reconnection |

Must be stubbed as a no-op bidi stream to prevent infinite retry loops.

| # | Method | Type | Notes |
|---|--------|------|-------|
| 1 | Subscribe | bidi | Persistent push notification stream |
| 2 | GetPushTokens | unary | Get push tokens for apps |


### 12. PartnerTokenRPCService — Third-Party Service Tokens

| | |
|---|---|
| **Proto service** | `humane.partnerservices.PartnerTokenRPCService` |
| **Gateway** | API |
| **Source** | `humane/partnerservices/PartnerTokenRPCServiceGrpc.java` |
| **Stub created in** | `AppController.java` (boot) → `DeviceUserTokenService` → `PartnerServicesAccessManager` |
| **Stub types** | async |
| **Trigger** | On-demand (when partner tokens needed, e.g., music streaming) |

| # | Method | Type | Notes |
|---|--------|------|-------|
| 1 | GetToken | unary | Get single partner token |
| 2 | GetTokens | unary | Get multiple partner tokens |


### 13. WifiConfigService — WiFi Network Sync

| | |
|---|---|
| **Proto service** | `humane.account.WifiConfigService` |
| **Gateway** | API |
| **Source** | `humane/account/WifiConfigServiceGrpc.java` |
| **Stub created in** | `AppController.java` (boot) → `WifiConfigService` → `WifiConfigServiceManager` |
| **Stub types** | async |
| **Trigger** | Periodic (every 4 hours via WorkManager); also at boot |

| # | Method | Type | Notes |
|---|--------|------|-------|
| 1 | ListSecureWifiConfigs | unary | Fetch WiFi configs from cloud |


### 14. DeviceOnboardingDACService — Provisioning

| | |
|---|---|
| **Proto service** | `humane.provisioning.DeviceOnboardingDACService` |
| **Gateway** | **ONBOARDING** |
| **Source** | `humane/provisioning/DeviceOnboardingDACServiceGrpc.java` |
| **Stub created in** | `ProvisioningService.java` (onCreate) |
| **Stub types** | async |
| **Trigger** | Provisioning/onboarding flow only (not normal operation) |

Not needed for normal operation. Only relevant during initial device setup.

| # | Method | Type | Notes |
|---|--------|------|-------|
| 1 | GetSubscriptionStatus | unary | Check subscription |
| 2 | GetAssignedUserDAC | unary | Get assigned user cert |
| 3 | VerifyHmcAssociation | unary | Verify HMC pairing |
| 4 | VerifyHmcByPass | unary | Verify HMC bypass |
| 5 | CreateLoginInit | unary | Login flow init |
| 6 | CreateLoginFinish | unary | Login flow finish |
| 7 | CreateDeviceUserBinding | unary | Bind user to device (encrypted) |


### 15. E911GeoLocationService — Emergency Location

| | |
|---|---|
| **Proto service** | `humane.location.v1.E911GeoLocationService` |
| **Gateway** | **E911GEO** |
| **Source** | `humane/location/v1/E911GeoLocationServiceGrpc.java` |
| **Stub created in** | `IGrandCentralServiceImpl.java` (lazy, on first call) |
| **Stub types** | async |
| **Trigger** | Emergency (911 calls only) |

Separate gateway, lazy connection. Only relevant during 911 calls.

| # | Method | Type | Notes |
|---|--------|------|-------|
| 1 | GeoLocate | unary | Emergency geolocation |


---

## Inactive Services (not instantiated on-device)

These proto definitions exist in the APK but no client stubs are created.
They're either backend-only, deprecated, or unreleased.

| Service | Package | Methods | Notes |
|---------|---------|---------|-------|
| FoodService | `humane.aibus` | EncryptedIdentifyFood, Feedback | On-device uses AIBusService.encryptedGetFoodItem instead |
| FoodPreferencesService | `humane.account` | Get/SetUserDailyIntakeGoals, Get/SetFoodRestrictions | Never instantiated |
| AmazonShoppingService | `humane.aibus` | VisualSearch | Never instantiated |
| DeviceMessagesService | `humane.aibus` | BackupMessages, QueryMessages, UploadAttachment | Never instantiated |
| WebSearchService | `humane.aibus` | search | Web search happens server-side as function execution |
| TestAutomationService | `humane.aibus` | Calendar test methods | Test-only |
| TestingAutomationService | `humane.capture` | Note test methods | Test-only |


---

## Stubbing Priority

### Must stub (boot stability + core function)

| Priority | Service | Why |
|----------|---------|-----|
| **P0** | PushRelayService/Subscribe | Persistent bidi stream at boot. Without a stub, enters infinite retry with exponential backoff — burns battery, floods logs |
| **P0** | AIBusService/Understand | The AI query path. Without this, voice queries get no response |
| **P0** | FeatureFlagsService/GetFlags | Called at boot + daily. Empty response is fine (all defaults) |

### Should stub (clean boot, no error noise)

| Priority | Service | Why |
|----------|---------|-----|
| **P1** | PushRelayService/GetPushTokens | Called at boot. Empty response is fine |
| **P1** | WifiConfigService/ListSecureWifiConfigs | Called at boot + every 4 hours. Empty response is fine |
| **P1** | UserInformationService/GetUserPersonalDetails | Called once at boot. Empty/stub response — user name defaults to empty |
| **P1** | ContactsRPCService/GetContacts | Called at boot for sync. Empty contact list is fine |
| **P1** | EventsIngestService/Ingest | Persistent bidi stream at boot. Accept and discard events |

### Nice to have (feature completeness)

| Priority | Service | Why |
|----------|---------|-----|
| **P2** | SpeechService/TextToSpeech | Remote TTS. Local Android TTS fallback works, but cloud TTS sounds better |
| **P2** | CaptureService/* | Photo/video features |
| **P2** | CompositionService/* | Message drafting, notification summary |
| **P2** | ContactsRPCService/* (remaining) | Full contact sync |
| **P2** | PartnerTokenRPCService | Third-party integrations (music, etc.) |

### Not needed

| Priority | Service | Why |
|----------|---------|-----|
| **P3** | PublicPrivacyService | Krypton is disabled (`DISABLE_APP_ENCRYPTION = true`) |
| **P3** | DeviceOnboardingDACService | Onboarding is already bypassed |
| **P3** | E911GeoLocationService | Separate gateway, emergency only |
| **P3** | DeviceEventsHistoryService | Telemetry sync, non-functional |


---

## Call Pattern at Boot

Approximate order of gRPC activity after ironman starts:

1. `PushRelayService/Subscribe` — bidi stream opened immediately by `PushService`
2. `FeatureFlagsService/GetFlags` — WorkManager job fires
3. `WifiConfigService/ListSecureWifiConfigs` — initial WiFi config sync
4. `UserInformationService/GetUserPersonalDetails` — one-time name fetch
5. `ContactsRPCService/GetContacts` — initial contact sync
6. `EventsIngestService/Ingest` — persistent telemetry stream
7. (user holds touchpad) `AIBusService/Understand` — AI query


## Error Behavior Summary

| Error Code | Effect |
|------------|--------|
| `UNIMPLEMENTED` | Safe. No auth check, no side effects. Default for unstubbed RPCs |
| `OK` (any success) | Calls `resetAuthStateIfNeeded()` — re-authorizes if previously de-authed |
| `UNAVAILABLE` | Normal transient error. Client may retry |
| `DEADLINE_EXCEEDED` | Normal timeout. Client handles gracefully |
| `PERMISSION_DENIED` + `unauthorized-device` trailer | **DANGEROUS** — locks device, engages keyguard |
| `UNAUTHENTICATED` + `subscription-status` trailer | **DANGEROUS** — revokes subscription |

**Rule: Never return `PERMISSION_DENIED` or `UNAUTHENTICATED` from the mock server.**
