// This file was generated by [ts-rs](https://github.com/Aleph-Alpha/ts-rs). Do not edit this file manually.
import type { CommandOptionType } from "./CommandOptionType";

export type CommandInteractionOptionValue = { kind: "string", value: string, } | { kind: "integer", value: bigint, } | { kind: "boolean", value: boolean, } | { kind: "user", value: string, } | { kind: "channel", value: string, } | { kind: "role", value: string, } | { kind: "mentionable", value: string, } | { kind: "number", value: number, } | { kind: "focused", value: string, option_kind: CommandOptionType, };