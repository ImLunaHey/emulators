//! Xbox kernel (`xboxkrnl.exe`) export table: ordinal -> (name, stdcall arg
//! bytes). This is pure reference data used by the HLE kernel ([`crate::hle`]) to
//! name imports and to clean the right number of bytes off the stack on return
//! (the Xbox kernel is stdcall: the callee pops its arguments).
//!
//! Source of truth: the official retail `xboxkrnl.exe` export DEF file from the
//! XboxDev/nxdk SDK (`lib/xboxkrnl/xboxkrnl.exe.def`), which encodes the stdcall
//! argument byte count directly in each decorated export name (e.g.
//! `AvSendTVEncoderOption@16` => 16 arg bytes). Names cross-checked against the
//! Cxbx-Reloaded kernel headers and xboxdevwiki. The retail kernel exports
//! ordinals 1..=378 with 367..=373 absent (those are debug/profiling-only stubs).
//!
//! Notes on the third field (arg bytes):
//! - It is the total stdcall argument size in bytes (`num_dword_args * 4`,
//!   counting any 64-bit-by-value LARGE_INTEGER argument as 8).
//! - DATA exports (kernel variables, not functions) carry 0.
//! - A handful of fastcall exports (Kf*/Exf*/Iof*/Interlocked*/Obf*) pass their
//!   first two dword args in ecx/edx; the byte count here is still the full
//!   decorated arg size. Callers that emulate stdcall stack cleanup must account
//!   for the register-passed args separately (see per-entry comments).
//! - The Rtl*printf / DbgPrint exports are cdecl + variadic (caller cleans the
//!   stack); 0 callee-pop bytes (flagged TODO).

/// `(ordinal, name, arg_byte_count)`. Sorted by ordinal.
pub const KERNEL_EXPORTS: &[(u16, &str, u16)] = &[
    (1, "AvGetSavedDataAddress", 0),
    (2, "AvSendTVEncoderOption", 16),
    (3, "AvSetDisplayMode", 24),
    (4, "AvSetSavedDataAddress", 4),
    (5, "DbgBreakPoint", 0),
    (6, "DbgBreakPointWithStatus", 4),
    (7, "DbgLoadImageSymbols", 12),
    (8, "DbgPrint", 0), // TODO: verify args (variadic/cdecl: caller cleans stack, 0 callee pop)
    (9, "HalReadSMCTrayState", 8),
    (10, "DbgPrompt", 12),
    (11, "DbgUnLoadImageSymbols", 12),
    (12, "ExAcquireReadWriteLockExclusive", 4),
    (13, "ExAcquireReadWriteLockShared", 4),
    (14, "ExAllocatePool", 4),
    (15, "ExAllocatePoolWithTag", 8),
    (16, "ExEventObjectType", 0), // DATA export (no args)
    (17, "ExFreePool", 4),
    (18, "ExInitializeReadWriteLock", 4),
    (19, "ExInterlockedAddLargeInteger", 16),
    (20, "ExInterlockedAddLargeStatistic", 8), // fastcall: @N is total arg bytes; first 2 dwords passed in ecx/edx
    (21, "ExInterlockedCompareExchange64", 12), // fastcall: @N is total arg bytes; first 2 dwords passed in ecx/edx
    (22, "ExMutantObjectType", 0), // DATA export (no args)
    (23, "ExQueryPoolBlockSize", 4),
    (24, "ExQueryNonVolatileSetting", 20),
    (25, "ExReadWriteRefurbInfo", 12),
    (26, "ExRaiseException", 4),
    (27, "ExRaiseStatus", 4),
    (28, "ExReleaseReadWriteLock", 4),
    (29, "ExSaveNonVolatileSetting", 16),
    (30, "ExSemaphoreObjectType", 0), // DATA export (no args)
    (31, "ExTimerObjectType", 0), // DATA export (no args)
    (32, "ExfInterlockedInsertHeadList", 8), // fastcall: @N is total arg bytes; first 2 dwords passed in ecx/edx
    (33, "ExfInterlockedInsertTailList", 8), // fastcall: @N is total arg bytes; first 2 dwords passed in ecx/edx
    (34, "ExfInterlockedRemoveHeadList", 4), // fastcall: @N is total arg bytes; first 2 dwords passed in ecx/edx
    (35, "FscGetCacheSize", 0),
    (36, "FscInvalidateIdleBlocks", 0),
    (37, "FscSetCacheSize", 4),
    (38, "HalClearSoftwareInterrupt", 4), // fastcall: @N is total arg bytes; first 2 dwords passed in ecx/edx
    (39, "HalDisableSystemInterrupt", 4),
    (40, "HalDiskCachePartitionCount", 0), // DATA export (no args)
    (41, "HalDiskModelNumber", 0), // DATA export (no args)
    (42, "HalDiskSerialNumber", 0), // DATA export (no args)
    (43, "HalEnableSystemInterrupt", 8),
    (44, "HalGetInterruptVector", 8),
    (45, "HalReadSMBusValue", 16),
    (46, "HalReadWritePCISpace", 24),
    (47, "HalRegisterShutdownNotification", 8),
    (48, "HalRequestSoftwareInterrupt", 4), // fastcall: @N is total arg bytes; first 2 dwords passed in ecx/edx
    (49, "HalReturnToFirmware", 4),
    (50, "HalWriteSMBusValue", 16),
    (51, "InterlockedCompareExchange", 12), // fastcall: @N is total arg bytes; first 2 dwords passed in ecx/edx
    (52, "InterlockedDecrement", 4), // fastcall: @N is total arg bytes; first 2 dwords passed in ecx/edx
    (53, "InterlockedIncrement", 4), // fastcall: @N is total arg bytes; first 2 dwords passed in ecx/edx
    (54, "InterlockedExchange", 8), // fastcall: @N is total arg bytes; first 2 dwords passed in ecx/edx
    (55, "InterlockedExchangeAdd", 8), // fastcall: @N is total arg bytes; first 2 dwords passed in ecx/edx
    (56, "InterlockedFlushSList", 4), // fastcall: @N is total arg bytes; first 2 dwords passed in ecx/edx
    (57, "InterlockedPopEntrySList", 4), // fastcall: @N is total arg bytes; first 2 dwords passed in ecx/edx
    (58, "InterlockedPushEntrySList", 8), // fastcall: @N is total arg bytes; first 2 dwords passed in ecx/edx
    (59, "IoAllocateIrp", 4),
    (60, "IoBuildAsynchronousFsdRequest", 24),
    (61, "IoBuildDeviceIoControlRequest", 36),
    (62, "IoBuildSynchronousFsdRequest", 28),
    (63, "IoCheckShareAccess", 20),
    (64, "IoCompletionObjectType", 0), // DATA export (no args)
    (65, "IoCreateDevice", 24),
    (66, "IoCreateFile", 40),
    (67, "IoCreateSymbolicLink", 8),
    (68, "IoDeleteDevice", 4),
    (69, "IoDeleteSymbolicLink", 4),
    (70, "IoDeviceObjectType", 0), // DATA export (no args)
    (71, "IoFileObjectType", 0), // DATA export (no args)
    (72, "IoFreeIrp", 4),
    (73, "IoInitializeIrp", 12),
    (74, "IoInvalidDeviceRequest", 8),
    (75, "IoQueryFileInformation", 20),
    (76, "IoQueryVolumeInformation", 20),
    (77, "IoQueueThreadIrp", 4),
    (78, "IoRemoveShareAccess", 8),
    (79, "IoSetIoCompletion", 20),
    (80, "IoSetShareAccess", 16),
    (81, "IoStartNextPacket", 4),
    (82, "IoStartNextPacketByKey", 8),
    (83, "IoStartPacket", 12),
    (84, "IoSynchronousDeviceIoControlRequest", 32),
    (85, "IoSynchronousFsdRequest", 20),
    (86, "IofCallDriver", 8), // fastcall: @N is total arg bytes; first 2 dwords passed in ecx/edx
    (87, "IofCompleteRequest", 8), // fastcall: @N is total arg bytes; first 2 dwords passed in ecx/edx
    (88, "KdDebuggerEnabled", 0), // DATA export (no args)
    (89, "KdDebuggerNotPresent", 0), // DATA export (no args)
    (90, "IoDismountVolume", 4),
    (91, "IoDismountVolumeByName", 4),
    (92, "KeAlertResumeThread", 4),
    (93, "KeAlertThread", 8),
    (94, "KeBoostPriorityThread", 8),
    (95, "KeBugCheck", 4),
    (96, "KeBugCheckEx", 20),
    (97, "KeCancelTimer", 4),
    (98, "KeConnectInterrupt", 4),
    (99, "KeDelayExecutionThread", 12),
    (100, "KeDisconnectInterrupt", 4),
    (101, "KeEnterCriticalRegion", 0),
    (102, "MmGlobalData", 0), // DATA export (no args)
    (103, "KeGetCurrentIrql", 0),
    (104, "KeGetCurrentThread", 0),
    (105, "KeInitializeApc", 28),
    (106, "KeInitializeDeviceQueue", 4),
    (107, "KeInitializeDpc", 12),
    (108, "KeInitializeEvent", 12),
    (109, "KeInitializeInterrupt", 28),
    (110, "KeInitializeMutant", 8),
    (111, "KeInitializeQueue", 8),
    (112, "KeInitializeSemaphore", 12),
    (113, "KeInitializeTimerEx", 8),
    (114, "KeInsertByKeyDeviceQueue", 12),
    (115, "KeInsertDeviceQueue", 8),
    (116, "KeInsertHeadQueue", 8),
    (117, "KeInsertQueue", 8),
    (118, "KeInsertQueueApc", 16),
    (119, "KeInsertQueueDpc", 12),
    (120, "KeInterruptTime", 0), // DATA export (no args)
    (121, "KeIsExecutingDpc", 0),
    (122, "KeLeaveCriticalRegion", 0),
    (123, "KePulseEvent", 12),
    (124, "KeQueryBasePriorityThread", 4),
    (125, "KeQueryInterruptTime", 0),
    (126, "KeQueryPerformanceCounter", 0),
    (127, "KeQueryPerformanceFrequency", 0),
    (128, "KeQuerySystemTime", 4),
    (129, "KeRaiseIrqlToDpcLevel", 0),
    (130, "KeRaiseIrqlToSynchLevel", 0),
    (131, "KeReleaseMutant", 16),
    (132, "KeReleaseSemaphore", 16),
    (133, "KeRemoveByKeyDeviceQueue", 8),
    (134, "KeRemoveDeviceQueue", 4),
    (135, "KeRemoveEntryDeviceQueue", 8),
    (136, "KeRemoveQueue", 12),
    (137, "KeRemoveQueueDpc", 4),
    (138, "KeResetEvent", 4),
    (139, "KeRestoreFloatingPointState", 4),
    (140, "KeResumeThread", 4),
    (141, "KeRundownQueue", 4),
    (142, "KeSaveFloatingPointState", 4),
    (143, "KeSetBasePriorityThread", 8),
    (144, "KeSetDisableBoostThread", 8),
    (145, "KeSetEvent", 12),
    (146, "KeSetEventBoostPriority", 8),
    (147, "KeSetPriorityProcess", 8),
    (148, "KeSetPriorityThread", 8),
    (149, "KeSetTimer", 16),
    (150, "KeSetTimerEx", 20),
    (151, "KeStallExecutionProcessor", 4),
    (152, "KeSuspendThread", 4),
    (153, "KeSynchronizeExecution", 12),
    (154, "KeSystemTime", 0), // DATA export (no args)
    (155, "KeTestAlertThread", 4),
    (156, "KeTickCount", 0), // DATA export (no args)
    (157, "KeTimeIncrement", 0), // DATA export (no args)
    (158, "KeWaitForMultipleObjects", 32),
    (159, "KeWaitForSingleObject", 20),
    (160, "KfRaiseIrql", 4), // fastcall: @N is total arg bytes; first 2 dwords passed in ecx/edx
    (161, "KfLowerIrql", 4), // fastcall: @N is total arg bytes; first 2 dwords passed in ecx/edx
    (162, "KiBugCheckData", 0), // DATA export (no args)
    (163, "KiUnlockDispatcherDatabase", 4), // fastcall: @N is total arg bytes; first 2 dwords passed in ecx/edx
    (164, "LaunchDataPage", 0), // DATA export (no args)
    (165, "MmAllocateContiguousMemory", 4),
    (166, "MmAllocateContiguousMemoryEx", 20),
    (167, "MmAllocateSystemMemory", 8),
    (168, "MmClaimGpuInstanceMemory", 8),
    (169, "MmCreateKernelStack", 8),
    (170, "MmDeleteKernelStack", 8),
    (171, "MmFreeContiguousMemory", 4),
    (172, "MmFreeSystemMemory", 8),
    (173, "MmGetPhysicalAddress", 4),
    (174, "MmIsAddressValid", 4),
    (175, "MmLockUnlockBufferPages", 12),
    (176, "MmLockUnlockPhysicalPage", 8),
    (177, "MmMapIoSpace", 12),
    (178, "MmPersistContiguousMemory", 12),
    (179, "MmQueryAddressProtect", 4),
    (180, "MmQueryAllocationSize", 4),
    (181, "MmQueryStatistics", 4),
    (182, "MmSetAddressProtect", 12),
    (183, "MmUnmapIoSpace", 8),
    (184, "NtAllocateVirtualMemory", 20),
    (185, "NtCancelTimer", 8),
    (186, "NtClearEvent", 4),
    (187, "NtClose", 4),
    (188, "NtCreateDirectoryObject", 8),
    (189, "NtCreateEvent", 16),
    (190, "NtCreateFile", 36),
    (191, "NtCreateIoCompletion", 16),
    (192, "NtCreateMutant", 12),
    (193, "NtCreateSemaphore", 16),
    (194, "NtCreateTimer", 12),
    (195, "NtDeleteFile", 4),
    (196, "NtDeviceIoControlFile", 40),
    (197, "NtDuplicateObject", 12),
    (198, "NtFlushBuffersFile", 8),
    (199, "NtFreeVirtualMemory", 12),
    (200, "NtFsControlFile", 40),
    (201, "NtOpenDirectoryObject", 8),
    (202, "NtOpenFile", 24),
    (203, "NtOpenSymbolicLinkObject", 8),
    (204, "NtProtectVirtualMemory", 16),
    (205, "NtPulseEvent", 8),
    (206, "NtQueueApcThread", 20),
    (207, "NtQueryDirectoryFile", 40),
    (208, "NtQueryDirectoryObject", 24),
    (209, "NtQueryEvent", 8),
    (210, "NtQueryFullAttributesFile", 8),
    (211, "NtQueryInformationFile", 20),
    (212, "NtQueryIoCompletion", 8),
    (213, "NtQueryMutant", 8),
    (214, "NtQuerySemaphore", 8),
    (215, "NtQuerySymbolicLinkObject", 12),
    (216, "NtQueryTimer", 8),
    (217, "NtQueryVirtualMemory", 8),
    (218, "NtQueryVolumeInformationFile", 20),
    (219, "NtReadFile", 32),
    (220, "NtReadFileScatter", 32),
    (221, "NtReleaseMutant", 8),
    (222, "NtReleaseSemaphore", 12),
    (223, "NtRemoveIoCompletion", 20),
    (224, "NtResumeThread", 8),
    (225, "NtSetEvent", 8),
    (226, "NtSetInformationFile", 20),
    (227, "NtSetIoCompletion", 20),
    (228, "NtSetSystemTime", 8),
    (229, "NtSetTimerEx", 32),
    (230, "NtSignalAndWaitForSingleObjectEx", 20),
    (231, "NtSuspendThread", 8),
    (232, "NtUserIoApcDispatcher", 12),
    (233, "NtWaitForSingleObject", 12),
    (234, "NtWaitForSingleObjectEx", 16),
    (235, "NtWaitForMultipleObjectsEx", 24),
    (236, "NtWriteFile", 32),
    (237, "NtWriteFileGather", 32),
    (238, "NtYieldExecution", 0),
    (239, "ObCreateObject", 16),
    (240, "ObDirectoryObjectType", 0), // DATA export (no args)
    (241, "ObInsertObject", 16),
    (242, "ObMakeTemporaryObject", 4),
    (243, "ObOpenObjectByName", 16),
    (244, "ObOpenObjectByPointer", 12),
    (245, "ObpObjectHandleTable", 0), // DATA export (no args)
    (246, "ObReferenceObjectByHandle", 12),
    (247, "ObReferenceObjectByName", 20),
    (248, "ObReferenceObjectByPointer", 8),
    (249, "ObSymbolicLinkObjectType", 0), // DATA export (no args)
    (250, "ObfDereferenceObject", 4), // fastcall: @N is total arg bytes; first 2 dwords passed in ecx/edx
    (251, "ObfReferenceObject", 4), // fastcall: @N is total arg bytes; first 2 dwords passed in ecx/edx
    (252, "PhyGetLinkState", 4),
    (253, "PhyInitialize", 8),
    (254, "PsCreateSystemThread", 20),
    (255, "PsCreateSystemThreadEx", 40),
    (256, "PsQueryStatistics", 4),
    (257, "PsSetCreateThreadNotifyRoutine", 4),
    (258, "PsTerminateSystemThread", 4),
    (259, "PsThreadObjectType", 0), // DATA export (no args)
    (260, "RtlAnsiStringToUnicodeString", 12),
    (261, "RtlAppendStringToString", 8),
    (262, "RtlAppendUnicodeStringToString", 8),
    (263, "RtlAppendUnicodeToString", 8),
    (264, "RtlAssert", 16),
    (265, "RtlCaptureContext", 4),
    (266, "RtlCaptureStackBackTrace", 16),
    (267, "RtlCharToInteger", 12),
    (268, "RtlCompareMemory", 12),
    (269, "RtlCompareMemoryUlong", 12),
    (270, "RtlCompareString", 12),
    (271, "RtlCompareUnicodeString", 12),
    (272, "RtlCopyString", 8),
    (273, "RtlCopyUnicodeString", 8),
    (274, "RtlCreateUnicodeString", 8),
    (275, "RtlDowncaseUnicodeChar", 4),
    (276, "RtlDowncaseUnicodeString", 12),
    (277, "RtlEnterCriticalSection", 4),
    (278, "RtlEnterCriticalSectionAndRegion", 4),
    (279, "RtlEqualString", 12),
    (280, "RtlEqualUnicodeString", 12),
    (281, "RtlExtendedIntegerMultiply", 12),
    (282, "RtlExtendedLargeIntegerDivide", 16),
    (283, "RtlExtendedMagicDivide", 20),
    (284, "RtlFillMemory", 12),
    (285, "RtlFillMemoryUlong", 12),
    (286, "RtlFreeAnsiString", 4),
    (287, "RtlFreeUnicodeString", 4),
    (288, "RtlGetCallersAddress", 8),
    (289, "RtlInitAnsiString", 8),
    (290, "RtlInitUnicodeString", 8),
    (291, "RtlInitializeCriticalSection", 4),
    (292, "RtlIntegerToChar", 16),
    (293, "RtlIntegerToUnicodeString", 12),
    (294, "RtlLeaveCriticalSection", 4),
    (295, "RtlLeaveCriticalSectionAndRegion", 4),
    (296, "RtlLowerChar", 4),
    (297, "RtlMapGenericMask", 8),
    (298, "RtlMoveMemory", 12),
    (299, "RtlMultiByteToUnicodeN", 20),
    (300, "RtlMultiByteToUnicodeSize", 12),
    (301, "RtlNtStatusToDosError", 4),
    (302, "RtlRaiseException", 4),
    (303, "RtlRaiseStatus", 4),
    (304, "RtlTimeFieldsToTime", 8),
    (305, "RtlTimeToTimeFields", 8),
    (306, "RtlTryEnterCriticalSection", 4),
    (307, "RtlUlongByteSwap", 4), // fastcall: @N is total arg bytes; first 2 dwords passed in ecx/edx
    (308, "RtlUnicodeStringToAnsiString", 12),
    (309, "RtlUnicodeStringToInteger", 12),
    (310, "RtlUnicodeToMultiByteN", 20),
    (311, "RtlUnicodeToMultiByteSize", 12),
    (312, "RtlUnwind", 16),
    (313, "RtlUpcaseUnicodeChar", 4),
    (314, "RtlUpcaseUnicodeString", 12),
    (315, "RtlUpcaseUnicodeToMultiByteN", 20),
    (316, "RtlUpperChar", 4),
    (317, "RtlUpperString", 8),
    (318, "RtlUshortByteSwap", 4), // fastcall: @N is total arg bytes; first 2 dwords passed in ecx/edx
    (319, "RtlWalkFrameChain", 12),
    (320, "RtlZeroMemory", 8),
    (321, "XboxEEPROMKey", 0), // DATA export (no args)
    (322, "XboxHardwareInfo", 0), // DATA export (no args)
    (323, "XboxHDKey", 0), // DATA export (no args)
    (324, "XboxKrnlVersion", 0), // DATA export (no args)
    (325, "XboxSignatureKey", 0), // DATA export (no args)
    (326, "XeImageFileName", 0), // DATA export (no args)
    (327, "XeLoadSection", 4),
    (328, "XeUnloadSection", 4),
    (329, "READ_PORT_BUFFER_UCHAR", 12),
    (330, "READ_PORT_BUFFER_USHORT", 12),
    (331, "READ_PORT_BUFFER_ULONG", 12),
    (332, "WRITE_PORT_BUFFER_UCHAR", 12),
    (333, "WRITE_PORT_BUFFER_USHORT", 12),
    (334, "WRITE_PORT_BUFFER_ULONG", 12),
    (335, "XcSHAInit", 4),
    (336, "XcSHAUpdate", 12),
    (337, "XcSHAFinal", 8),
    (338, "XcRC4Key", 12),
    (339, "XcRC4Crypt", 12),
    (340, "XcHMAC", 28),
    (341, "XcPKEncPublic", 12),
    (342, "XcPKDecPrivate", 12),
    (343, "XcPKGetKeyLen", 4),
    (344, "XcVerifyPKCS1Signature", 12),
    (345, "XcModExp", 20),
    (346, "XcDESKeyParity", 8),
    (347, "XcKeyTable", 12),
    (348, "XcBlockCrypt", 20),
    (349, "XcBlockCryptCBC", 28),
    (350, "XcCryptService", 8),
    (351, "XcUpdateCrypto", 8),
    (352, "RtlRip", 12),
    (353, "XboxLANKey", 0), // DATA export (no args)
    (354, "XboxAlternateSignatureKeys", 0), // DATA export (no args)
    (355, "XePublicKeyData", 0), // DATA export (no args)
    (356, "HalBootSMCVideoMode", 0), // DATA export (no args)
    (357, "IdexChannelObject", 0), // DATA export (no args)
    (358, "HalIsResetOrShutdownPending", 0),
    (359, "IoMarkIrpMustComplete", 4),
    (360, "HalInitiateShutdown", 0),
    (361, "RtlSnprintf", 0), // TODO: verify args (variadic/cdecl: caller cleans stack, 0 callee pop)
    (362, "RtlSprintf", 0), // TODO: verify args (variadic/cdecl: caller cleans stack, 0 callee pop)
    (363, "RtlVsnprintf", 0), // TODO: verify args (variadic/cdecl: caller cleans stack, 0 callee pop)
    (364, "RtlVsprintf", 0), // TODO: verify args (variadic/cdecl: caller cleans stack, 0 callee pop)
    (365, "HalEnableSecureTrayEject", 0),
    (366, "HalWriteSMCScratchRegister", 4),
    (374, "MmDbgAllocateMemory", 8),
    (375, "MmDbgFreeMemory", 8),
    (376, "MmDbgQueryAvailablePages", 0),
    (377, "MmDbgReleaseAddress", 8),
    (378, "MmDbgWriteCheck", 8),
];

/// Look up a kernel export by ordinal. Returns `(name, arg_byte_count)` -- e.g.
/// a 3-DWORD-argument stdcall function returns `("Foo", 12)`. Returns `None` for
/// ordinals the retail kernel does not export.
pub fn lookup(ordinal: u32) -> Option<(&'static str, u16)> {
    // Ordinals are sorted, so a binary search is exact and cheap; a linear
    // `find` would be fine too given the table size.
    KERNEL_EXPORTS
        .binary_search_by(|e| (e.0 as u32).cmp(&ordinal))
        .ok()
        .map(|i| (KERNEL_EXPORTS[i].1, KERNEL_EXPORTS[i].2))
}

/// Ordinals that are DATA exports (kernel variables, not callable functions).
/// Their import thunks must point at real backing memory, not call stubs.
pub const DATA_EXPORTS: &[u16] = &[
    16, 22, 30, 31, 40, 41, 42, 64, 70, 71, 88, 89, 102, 120, 154, 156, 157, 162,
    164, 240, 245, 249, 259, 321, 322, 323, 324, 325, 326, 353, 354, 355, 356, 357,
];

/// Whether `ordinal` is a DATA export (a kernel variable, read as memory).
pub fn is_data_export(ordinal: u32) -> bool {
    DATA_EXPORTS.contains(&(ordinal as u16))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn well_known_ordinals_resolve() {
        assert_eq!(lookup(1), Some(("AvGetSavedDataAddress", 0)));
        assert_eq!(lookup(2), Some(("AvSendTVEncoderOption", 16)));
        assert_eq!(lookup(3), Some(("AvSetDisplayMode", 24)));
        assert_eq!(lookup(4), Some(("AvSetSavedDataAddress", 4)));
        assert_eq!(lookup(8), Some(("DbgPrint", 0)));
        assert_eq!(lookup(14), Some(("ExAllocatePool", 4)));
        assert_eq!(lookup(160), Some(("KfRaiseIrql", 4)));
        assert_eq!(lookup(164), Some(("LaunchDataPage", 0)));
        assert_eq!(lookup(165), Some(("MmAllocateContiguousMemory", 4)));
        // 204/205 are a known typo trap in some headers; verify the retail order.
        assert_eq!(lookup(204), Some(("NtProtectVirtualMemory", 16)));
        assert_eq!(lookup(205), Some(("NtPulseEvent", 8)));
        assert_eq!(lookup(255), Some(("PsCreateSystemThreadEx", 40)));
        assert_eq!(lookup(335), Some(("XcSHAInit", 4)));
    }

    #[test]
    fn out_of_range_returns_none() {
        assert_eq!(lookup(0), None);
        assert_eq!(lookup(9999), None);
        // Gaps within the retail range are absent too.
        assert_eq!(lookup(367), None);
        assert_eq!(lookup(373), None);
    }

    #[test]
    fn no_duplicate_ordinals() {
        let mut seen = std::collections::BTreeSet::new();
        for e in KERNEL_EXPORTS {
            assert!(seen.insert(e.0), "duplicate ordinal {}", e.0);
        }
    }

    #[test]
    fn table_is_sorted_by_ordinal() {
        // `lookup` relies on the table being sorted for its binary search.
        assert!(KERNEL_EXPORTS.windows(2).all(|w| w[0].0 < w[1].0));
    }

    #[test]
    fn halo2_imported_ordinals_all_present() {
        // The 152 ordinals Halo 2 imports from xboxkrnl.exe; all must resolve.
        const HALO2: &[u16] = &[
            178, 165, 164, 171, 301, 49, 67, 69, 289, 187, 215, 203, 337, 336, 335,
            326, 166, 180, 182, 179, 218, 202, 236, 211, 190, 226, 128, 322, 47, 324,
            197, 294, 277, 250, 143, 246, 259, 224, 302, 238, 258, 255, 233, 219, 198,
            200, 196, 172, 167, 340, 323, 199, 184, 269, 291, 217, 279, 327, 328, 24,
            40, 149, 161, 129, 360, 113, 107, 145, 357, 159, 189, 16, 225, 193, 222,
            192, 221, 234, 235, 99, 325, 354, 207, 210, 312, 95, 142, 139, 181, 65,
            74, 305, 156, 304, 260, 308, 356, 97, 83, 87, 81, 17, 15, 359, 358, 44,
            160, 98, 109, 151, 173, 175, 119, 176, 1, 2, 4, 3, 100, 46, 168, 8, 23,
            137, 153, 170, 169, 339, 338, 349, 347, 346, 345, 252, 253, 158, 344, 343,
            195, 228, 150, 355, 353, 86, 62, 247, 126, 127, 42, 41, 85, 84,
        ];
        for &o in HALO2 {
            assert!(lookup(o as u32).is_some(), "missing Halo 2 ordinal {o}");
        }
    }
}
