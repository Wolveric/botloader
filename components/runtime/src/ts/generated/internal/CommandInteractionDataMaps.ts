// This file was generated by [ts-rs](https://github.com/Aleph-Alpha/ts-rs). Do not edit this file manually.
import type { IMessage } from "./IMessage";
import type { IUser } from "./IUser";
import type { InteractionPartialChannel } from "./InteractionChannel";
import type { InteractionPartialMember } from "./InteractionPartialMember";
import type { Role } from "../discord/Role";

export interface CommandInteractionDataMap { channels: Record<string, InteractionPartialChannel>, members: Record<string, InteractionPartialMember>, messages: Record<string, IMessage>, roles: Record<string, Role>, users: Record<string, IUser>, }