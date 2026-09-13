#pragma once
#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// Windows x64 C ABI. Resolve these names with GetProcAddress or use t7patch.dll.lib.
// Call outside DllMain, after the game's Steam/Demonware/lobby objects are initialized.
void EnableInjectorlessInstall(void);
void zbr_run_gamemode_lui(const char* input); // "serious_anticrash_2023"
void SetFriendsOnly(bool enabled);
void SetPlayerName(const char* name);        // <= 15 bytes, NUL terminated
void SetNetworkPassword(const char* password); // empty/NULL clears it
void Unload(void);                         // logical deactivation; module stays pinned

// Optional launcher API v1. Callable as a Windows x64 thread entry point.
// Status: 1=waiting, 2=active, 3=unsupported, 4=failed, 5=deactivated, 6=bad request.
typedef struct T7PatchStartRequest {
    uint32_t size;       // sizeof(T7PatchStartRequest)
    uint32_t version;    // 1
    uint32_t status;     // written by DLL
    uint32_t reserved;   // 0
    uint16_t config_path[1024]; // absolute UTF-16 path, NUL terminated
    uint16_t message[256];      // written by DLL, UTF-16
} T7PatchStartRequest;
uint32_t T7PatchStart(void* request); // request must remain writable until the call returns

// Optional data export "T7PatchStatus": aligned 4-byte lifecycle state, written
// atomically by the DLL. Resolve its address and use ReadProcessMemory; never write
// to it or call it as a function. Poll after ACTIVE without creating remote threads.
// Values: 1=not installed yet, 2=active, 4=failed install, 5=deactivated.

#ifdef __cplusplus
}
#endif
