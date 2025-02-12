// This file was generated by [ts-rs](https://github.com/Aleph-Alpha/ts-rs). Do not edit this file manually.
import type { IPermissionOverwrite } from "../discord/IPermissionOverwrite";
import type { VideoQualityMode } from "../discord/VideoQualityMode";

export interface IVoiceChannel { bitrate: number, id: string, kind: 'Voice'|'StageVoice', name: string, parentId: string | null, permissionOverwrites: Array<IPermissionOverwrite>, position: number, rtcRegion: string | null, userLimit: number | null, videoQualityMode: VideoQualityMode | null, }