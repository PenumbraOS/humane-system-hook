# Agent Context

This project is a set of patches on top of Humane's Ai Pin locked down firmware. While we do not have root access, through a vulnerability we are able to install APKs with `system` level permissions, and thus can inject ourselves directly into `system_service` and therefore every app that starts up afterwards. We set up a base patch injector (`injector/`), which automatically takes installed APKs (only `hook/` for now) with the `com.penumbraos.hook.TARGET_PACKAGES` manifest property, and injects the provided classes into those hook packages.

## Device Constraints

- No root access. Only system UID (1000) and system_server process access.
- System SELinux is extremely locked down. Do not assume just because something is doable in AOSP it is doable here.
- We do NOT have access to set 99% of system properties. Do not suggest setting a prop.

## Decompiled Humane Source

All decompiled source from the device (Humane apps, libraries, core systems) can be found at `/Users/adam/code/aipin/apk-environment`.

### Example source directories

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

## Frida Access

Frida Gadget 17.9.1 is not packaged by default. To include it in the APK, build with `-PincludeFrida=true`:

```bash
./gradlew :hook:assembleRelease -PincludeFrida=true
```

When packaged, Frida Gadget is loaded into ironman's main process, listening on `0.0.0.0:27042` with `on_load: resume` and `on_port_conflict: pick-next`. The voiceinteractor sub-process gets the next available port (27043).

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

## AOSP Source (Android 12L Reference)

```
/Users/adam/code/aipin/android/platform_frameworks_base/
```

## SELinux Policy

```
/Users/adam/code/aipin/dump/lockedpin/system/etc/selinux/plat_sepolicy.cil
/Users/adam/code/aipin/dump/lockedpin/system_ext/etc/selinux/system_ext_sepolicy.cil
```
