// This file was generated by [ts-rs](https://github.com/Aleph-Alpha/ts-rs). Do not edit this file manually.
import type { OpStorageBucketSetCondition } from "./StorageBucketSetCondition";
import type { OpStorageBucketValue } from "./StorageBucketValue";

export interface OpStorageBucketSetIf { bucketName: string, key: string, value: OpStorageBucketValue, ttl?: number, cond: OpStorageBucketSetCondition, }