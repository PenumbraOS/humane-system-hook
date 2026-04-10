# Goal

## Objective

Stub all necessary Humane cloud/backend processes to get the Ai Pin into a fully functional "working" state independent of Humane's servers. The device should be able to operate with mocked or locally-served services replacing all cloud dependencies.

## Proof of Concept

Mock gRPC traffic sufficiently to send a text/LLM response through the full pipeline to the device's UI (laser ink display) and audio output (narrator TTS). This proves end-to-end control of the AI response path without any cloud connectivity.

---

## System Architecture

### Boot Sequence

```
Boot
  ├─ GrandCentralBootReceiver → GrandCentralService (local platform service)
  ├─ HumaneLocationBootBroadcastReceiver → HumaneLocationService (local)
  └─ PersistenceService
       └─ Checks Settings.Global DUC_PROVISIONED
            ├─ If 0: stuck at onboarding (needs DeviceOnboardingDACService — cloud)
            └─ If 1: binds CentralService
                 └─ AppController.initializeIfNeeded()
                      ├─ AIBusService (local gRPC client wrapper)
                      ├─ Krypto/DataProtection init (local)
                      ├─ NetworkManager + ChannelFactory (gRPC channel setup)
                      ├─ ExperienceManager (local)
                      ├─ Feature flag sync (cloud, async, non-blocking)
                      ├─ WiFi sync (cloud, async, non-blocking)
                      ├─ Push service (cloud, async, non-blocking)
                      └─ Broadcasts CENTRAL_STARTED
```

For an already-provisioned device, no cloud service must respond for the device to boot. All cloud calls are async and non-blocking. The device defaults to "authorized + subscribed" and only loses that status when the cloud actively revokes it via gRPC response headers.

### AI Request Pipeline

```
User speaks (touchpad hold)
  → Audio captured by HumaneVoiceSession
  → Transcribed by AndroidTranscriber
  → TaoEventDispatcher.onRequest()
  → InterpreterOrchestrator runs cascade:
      1. REGEX       — local pattern matching
      2. SEMANTIC    — local on-device model
      3. CONFIRMATION — yes/no handling
      4. SEQ2SEQ    — local seq2seq model
      5. SYNAPSE    — cloud gRPC (AIBusService.synapseUnderstanding())
      6. FALLBACK
  → First interpreter returning non-null wins
  → For questions/complex requests: falls through to SYNAPSE (cloud)
```

### AI Response Pipeline

```
Cloud returns SynapseUnderstandingResponse (server-streaming)
  → SynapseInterpreter parses SynapseChatTurn
  → JsonResolver resolves into typed Action (e.g. RespondAction)
  → Switchboard.routeAction()
      ├─ CENTRAL actions → CentralActionHandler (local, no cloud)
      ├─ AGENT_SETTINGS → SettingsAgent (cloud-backed Tao agent loop)
      └─ Everything else → ExperienceManager.routeAction()
           → Intent to target experience (e.g. Answers)
  → RespondActionHandler creates AnswerInteractor
      ├─ VISUAL: AnswerViewController → ResponseView → laser projector
      └─ AUDIO: NarratorAccess.speak() → HybridSpeechSynthesizer
           ├─ Remote TTS: AIBus textToSpeech() gRPC → audio playback
           └─ Local TTS: on-device Android TTS (fallback)
```

### Display/Output Paradigm

The Arbitrator mediates between audio and visual output:

- **Palm down (default):** response is narrated aloud via TTS
- **Palm raised (FlatHandService detects):** laser projector activates, narration is suppressed, response rendered on hand via ResponseView

### Experience Types

```
CENTRAL, AGENT_SETTINGS, ANSWERS, CONTACTS, DIALER, FOOD,
MESSAGES, MESSAGES_BACKGROUND, MUSIC, NOTIFICATIONS, PHOTOGRAPHY,
STATIC_DISPLAY, SETTINGS, TARGETS_GAME, TRANSLATION, UI_GALLERY,
VISUALIZER_DISPLAY, SYSTEM_NAVIGATION, CLOCK, VOICEMAIL, TICKLE_PROTOTYPE
```

### gRPC Layer Architecture

```
AIBusServiceGrpc (protobuf stubs, cloud)
    ↕ encrypt/decrypt via EphemeralProtectionManager (Krypton)
AIBusService (gRPC client wrapper)
    ↕ Android Binder IPC (IAiBusBridge AIDL)
AiBusBridge (Binder stub in Central process)
    ↕ Binder IPC
AIBusClient (Binder proxy in Brainstem process)
    ↕
Brainstem / TaoEventDispatcher
    ↕
Switchboard → ExperienceManager → Experience UI
```

### Encryption Layer (Krypton)

All AI Bus gRPC calls are encrypted at the application layer before being sent over gRPC. Each request type has its own `EphemeralChannelId`. The encryption uses AES-GCM with keys exchanged via Humane's Privacy Service.

A parallel test crypto infrastructure exists with a hardcoded test AES key and mock key management, accessible via `KryptoFactory.getTest*()` methods, but not wired into the production flow.

Non-AI-Bus gRPC services (feature flags, contacts, provisioning, etc.) do NOT use Krypton encryption. They use the gRPC channel directly.

### Gateway Configuration

Gateway URIs are loaded from `config_generated.json` in assets:

- `apiGatewayUri` → `api.prod.humane.cloud`
- `onboardingGatewayUri` → `onboarding.prod.humane.cloud`

**Override mechanism:** `BaseConfig.updateValuesFromSystemProperties()` checks Android system properties at startup and overrides config values via reflection. Properties follow the pattern `persist.humane.ironman.config.services.apiGatewayUri`.

**TLS behavior:** ChannelFactory checks the port. Port 443 uses mTLS with device certificates. Any other port uses `usePlaintext()` (no TLS). The plaintext path also adds an `X-Forwarded-Client-Cert` header marked "ForLocalDevelopment".

### Authorization Model

The device defaults to authorized + subscribed (status code UNSPECIFIED = subscribed). Authorization is reactive, not proactive. The `AccountAuthorizationInterceptor` checks gRPC response trailers. Only an explicit `PERMISSION_DENIED` or `UNAUTHENTICATED` response from the server marks the device as unauthorized. If the server is unreachable, the device retains its last known state.

### Local Actions (No Cloud Required)

CentralActionHandler handles these entirely on-device:

- Time, battery, online status, location, phone number
- Volume control, Bluetooth/airplane mode status
- WiFi connect/disconnect
- Privacy mode, lock/unlock instructions
- Factory reset, bug report
- Activity tracker start/stop
- Alert toggles (emergency, amber, public safety)

---

## Attack Surface

### Cloud Services (22 gRPC Stubs)

| Service | Package | Purpose |
|---------|---------|---------|
| AIBusService | humane.aibus | Main AI pipeline (25 RPCs: understand, chat completion, image analysis, weather, navigation, food, etc.) |
| SpeechService | humane.aibus | Text-to-speech |
| WebSearchService | humane.aibus | Web search |
| FoodService | humane.aibus | Food identification |
| CompositionService | humane.aibus | Message composition |
| AmazonShoppingService | humane.aibus | Shopping |
| DeviceMessagesService | humane.aibus | Device messages |
| TestAutomationService | humane.aibus | Test automation |
| CaptureService | humane.capture | Photo/video capture |
| TestingAutomationService | humane.capture | Capture testing |
| FoodPreferencesService | humane.account | Dietary preferences |
| UserInformationService | humane.account | User profile |
| WifiConfigService | humane.account | WiFi configuration sync |
| ContactsRPCService | humane.contacts | Contacts sync |
| DeviceEventsHistoryService | humane.events | Event history |
| EventsIngestService | humane.events | Event ingestion |
| FeatureFlagsService | humane.featureflags | Feature flags |
| E911GeoLocationService | humane.location.v1 | Emergency geolocation |
| PartnerTokenRPCService | humane.partnerservices | Partner service tokens |
| PublicPrivacyService | humane.privacy.grpc.pub | Privacy service |
| DeviceOnboardingDACService | humane.provisioning | Device provisioning (7 RPCs) |
| PushRelayService | humane.pushrelay | Push notifications |

### Interception Points (Cloud → UI)

From deepest (most transparent) to shallowest (easiest):

1. **gRPC transport** — redirect gateway URI to local server, serve real protobuf responses
2. **AIBusService** — hook/replace the gRPC client wrapper methods
3. **AiBusBridge / AIBusClient** — intercept at Binder IPC boundary
4. **SynapseInterpreter** — replace the cloud interpreter with a local one that returns fabricated SynapseChatTurn responses
5. **InterpreterOrchestrator** — add/modify a local interpreter (REGEX/SEMANTIC) that handles requests before SYNAPSE is reached
6. **Switchboard** — inject fabricated Action objects directly into the routing layer
7. **ExperienceManager.routeAction()** — inject a RespondAction directly into the Answers experience

### Minimum Path for a Response to Reach UI

A `RespondAction` carrying a response string, routed through `ExperienceManager.routeAction()` to the ANSWERS experience, where `RespondActionHandler` creates an `AnswerInteractor` (renders on laser display) and calls `NarratorAccess.speak()` (TTS output).

---

## Reference

### Decompiled ironman Source

```
/Users/adam/code/aipin/apk-environment/app/src/main/java/
  humaneinternal/system/                    — Core system (CentralService, AppController, etc.)
  humaneinternal/system/network/            — ChannelFactory, NetworkManager
  humaneinternal/system/aibus/              — AIBusService, AiBusBridge, AIBusClient
  humaneinternal/system/brainstem/          — AiBrainService (the Brainstem)
  humaneinternal/system/coordination/       — AppController, ExperienceManager, AiBridge
  humaneinternal/system/intent/             — LanguageUnderstanding, InterpreterOrchestrator
  humaneinternal/system/tao/                — TaoAgent, Arbitrator
  humaneinternal/system/narrator/           — NarratorImpl, HybridSpeechSynthesizer
  humaneinternal/system/krypto/             — KryptoFactory, EphemeralProtectionManager
  humaneinternal/system/config/             — BaseConfig, ServicesConfig
  humane/aibus/                             — AI Bus gRPC stubs + protobuf messages (467 files)
  humane/grandcentral/                      — GrandCentral service + separate ChannelFactory
  humane/experience/answers/                — Answers experience (RespondActionHandler)
```

### Injection Framework (This Project)

```
/Users/adam/code/aipin/openPin/humane-system-hook/
  hook/       — Loaded into ironman's process (AliuHook + Frida Gadget)
  injector/   — Runs in system_server (PMS mutation via reflection)
```

### Frida Access

Frida Gadget 17.9.1 is loaded into ironman's main process, listening on `0.0.0.0:27042` with `on_load: resume` and `on_port_conflict: pick-next`. The voiceinteractor sub-process gets the next available port (27043).

```bash
# Connect (replace <pin-ip> with device WiFi IP)
frida -H <pin-ip>:27042 Gadget

# List processes
frida-ps -H <pin-ip>:27042

# Run a script
frida -H <pin-ip>:27042 Gadget --eval 'Java.perform(() => { ... })'

# Enumerate all humane classes in the dex (not just loaded ones)
frida -H <pin-ip>:27042 Gadget -q --eval '
Java.perform(() => {
    var cl = Java.use("humaneinternal.system.MainApplication").class.getClassLoader();
    var DexFile = Java.use("dalvik.system.DexFile");
    var BaseDexClassLoader = Java.use("dalvik.system.BaseDexClassLoader");
    var loader = Java.cast(cl, BaseDexClassLoader);
    var pathListField = BaseDexClassLoader.class.getDeclaredField("pathList");
    pathListField.setAccessible(true);
    var pathList = pathListField.get(loader);
    var dexElementsField = pathList.getClass().getDeclaredField("dexElements");
    dexElementsField.setAccessible(true);
    var elements = dexElementsField.get(pathList);
    var Array = Java.use("java.lang.reflect.Array");
    var len = Array.getLength(elements);
    var allClasses = [];
    for (var i = 0; i < len; i++) {
        var element = Array.get(elements, i);
        if (element === null) continue;
        var dexFileField = element.getClass().getDeclaredField("dexFile");
        dexFileField.setAccessible(true);
        var dexFile = dexFileField.get(element);
        if (dexFile === null) continue;
        var df = Java.cast(dexFile, DexFile);
        var entries = df.entries();
        while (entries.hasMoreElements()) {
            var name = entries.nextElement().toString();
            if (name.includes("humane")) allClasses.push(name);
        }
    }
    allClasses.sort();
    allClasses.forEach(c => console.log(c));
});
'
```

Local frida CLI: `/Users/adam/.pyenv/versions/3.9.7/bin/frida` (v17.0.7, works despite version mismatch with gadget).

### AOSP Source (Android 12L Reference)

```
/Users/adam/code/aipin/android/platform_frameworks_base/
```

### SELinux Policy

```
/Users/adam/code/aipin/dump/lockedpin/system/etc/selinux/plat_sepolicy.cil
/Users/adam/code/aipin/dump/lockedpin/system_ext/etc/selinux/system_ext_sepolicy.cil
```
