// Generated from the local Rust service OpenAPI. Run npm run generate.
export interface paths {
    "/api/admin/v1/session": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Check current Access identity and application session */
        get: {
            parameters: {
                query?: never;
                header: {
                    /** @description RS256 Access application JWT for an explicitly allowed administrator */
                    "Cf-Access-Jwt-Assertion": string;
                };
                path?: never;
                cookie: {
                    "__Host-calrelay_admin": string;
                };
            };
            requestBody?: never;
            responses: {
                /** @description Authorized */
                204: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Denied, unavailable, expired, revoked or replayed; no secret diagnostics */
                403: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Denied, unavailable, expired, revoked or replayed; no secret diagnostics */
                503: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
            };
        };
        put?: never;
        /** Create an identity-bound browser session (optional trusted assembly) */
        post: {
            parameters: {
                query?: never;
                header: {
                    /** @description RS256 Access application JWT for an explicitly allowed administrator */
                    "Cf-Access-Jwt-Assertion": string;
                    /** @description Exact configured HTTPS origin */
                    Origin: string;
                };
                path?: never;
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        turnstile_token: string;
                    };
                };
            };
            responses: {
                /** @description Secure HttpOnly SameSite=Strict cookie; body tokens must stay in browser memory */
                200: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "Set-Cookie"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            csrf_token: string;
                            /** @description Unix seconds */
                            expires_at: number;
                            write_nonce: string;
                        };
                    };
                };
                /** @description Denied, unavailable, expired, revoked or replayed; no secret diagnostics */
                403: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Denied, unavailable, expired, revoked or replayed; no secret diagnostics */
                503: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
            };
        };
        /** Atomically revoke session and consume write nonce */
        delete: {
            parameters: {
                query?: never;
                header: {
                    /** @description RS256 Access application JWT for an explicitly allowed administrator */
                    "Cf-Access-Jwt-Assertion": string;
                    /** @description Exact configured HTTPS origin */
                    Origin: string;
                    "X-CSRF-Token": string;
                    "X-Admin-Write-Nonce": string;
                };
                path?: never;
                cookie: {
                    "__Host-calrelay_admin": string;
                };
            };
            requestBody?: never;
            responses: {
                /** @description Revoked; cookie cleared */
                204: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Denied, unavailable, expired, revoked or replayed; no secret diagnostics */
                403: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Denied, unavailable, expired, revoked or replayed; no secret diagnostics */
                503: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
            };
        };
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/ingest/v1/sources/{source_id}/events/{uid}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Source-scoped conditional machine write
         * @description Idempotency-Key is token-wide. Exactly one of If-Match (quoted positive revision) or If-None-Match: * is required. DELETE requires If-Match. Tombstones require their revision to resurrect. Retention expiry permits re-execution; unchanged writes do not advance revisions.
         */
        put: {
            parameters: {
                query?: never;
                header: {
                    "Idempotency-Key": string;
                    "If-Match"?: string;
                    "If-None-Match"?: string;
                };
                path: {
                    source_id: string;
                    uid: string;
                };
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        calendar_id: string;
                        /** @description EventV1: schemas/event-v1.schema.json; path UID must equal external_uid */
                        event: Record<string, never>;
                    };
                };
            };
            responses: {
                /** @description Committed or identical receipt replay */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Committed or identical receipt replay */
                201: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Token denied (source, action, expiry, revocation, disabled source) */
                401: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Conditional write, sequence or idempotency conflict */
                409: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Invalid request or event */
                422: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Persistence failed */
                500: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Stopping or bounded receipt capacity exhausted */
                503: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
            };
        };
        post?: never;
        /**
         * Source-scoped conditional machine write
         * @description Idempotency-Key is token-wide. Exactly one of If-Match (quoted positive revision) or If-None-Match: * is required. DELETE requires If-Match. Tombstones require their revision to resurrect. Retention expiry permits re-execution; unchanged writes do not advance revisions.
         */
        delete: {
            parameters: {
                query?: never;
                header: {
                    "Idempotency-Key": string;
                    "If-Match"?: string;
                    "If-None-Match"?: string;
                };
                path: {
                    source_id: string;
                    uid: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Committed or identical receipt replay */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Committed or identical receipt replay */
                201: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Token denied (source, action, expiry, revocation, disabled source) */
                401: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Conditional write, sequence or idempotency conflict */
                409: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Invalid request or event */
                422: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Persistence failed */
                500: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Stopping or bounded receipt capacity exhausted */
                503: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
            };
        };
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/openapi.json": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Success */
                200: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": Record<string, never>;
                    };
                };
                /** @description Bounded error; request_id matches X-Request-Id */
                400: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["Error"];
                    };
                };
                /** @description Bounded error; request_id matches X-Request-Id */
                404: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["Error"];
                    };
                };
                /** @description Bounded error; request_id matches X-Request-Id */
                405: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["Error"];
                    };
                };
                /** @description Bounded error; request_id matches X-Request-Id */
                408: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["Error"];
                    };
                };
                /** @description Bounded error; request_id matches X-Request-Id */
                413: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["Error"];
                    };
                };
                /** @description Bounded error; request_id matches X-Request-Id */
                500: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["Error"];
                    };
                };
                /** @description Bounded error; request_id matches X-Request-Id */
                503: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["Error"];
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/version": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Success */
                200: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @enum {string} */
                            api_version: "v1";
                            name: string;
                            version: string;
                        };
                    };
                };
                /** @description Bounded error; request_id matches X-Request-Id */
                400: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["Error"];
                    };
                };
                /** @description Bounded error; request_id matches X-Request-Id */
                404: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["Error"];
                    };
                };
                /** @description Bounded error; request_id matches X-Request-Id */
                405: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["Error"];
                    };
                };
                /** @description Bounded error; request_id matches X-Request-Id */
                408: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["Error"];
                    };
                };
                /** @description Bounded error; request_id matches X-Request-Id */
                413: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["Error"];
                    };
                };
                /** @description Bounded error; request_id matches X-Request-Id */
                500: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["Error"];
                    };
                };
                /** @description Bounded error; request_id matches X-Request-Id */
                503: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["Error"];
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/feeds/{public_id}/{secret}.ics": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Read a scoped single-source calendar
         * @description Revocable feed capability; no management session or human challenge. The full URL is secret and must not be logged.
         */
        get: {
            parameters: {
                query?: never;
                header?: {
                    "If-None-Match"?: string;
                };
                path: {
                    public_id: string;
                    secret: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Complete single-source ICS representation */
                200: {
                    headers: {
                        "Cache-Control"?: "private, no-cache";
                        ETag?: string;
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "text/calendar; charset=utf-8": string;
                    };
                };
                /** @description Unchanged authorized representation; no response body */
                304: {
                    headers: {
                        "Cache-Control"?: "private, no-cache";
                        ETag?: string;
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Fixed error envelope; no calendar content or ETag */
                400: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["Error"];
                    };
                };
                /** @description Fixed error envelope; no calendar content or ETag */
                404: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["Error"];
                    };
                };
                /** @description Fixed error envelope; no calendar content or ETag */
                405: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["Error"];
                    };
                };
                /** @description Fixed error envelope; no calendar content or ETag */
                408: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["Error"];
                    };
                };
                /** @description Fixed error envelope; no calendar content or ETag */
                413: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["Error"];
                    };
                };
                /** @description Fixed error envelope; no calendar content or ETag */
                422: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["Error"];
                    };
                };
                /** @description Fixed error envelope; no calendar content or ETag */
                500: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["Error"];
                    };
                };
                /** @description Fixed error envelope; no calendar content or ETag */
                503: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["Error"];
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        /**
         * Inspect an authorized calendar representation
         * @description Revocable feed capability; no management session or human challenge. The full URL is secret and must not be logged.
         */
        head: {
            parameters: {
                query?: never;
                header?: {
                    "If-None-Match"?: string;
                };
                path: {
                    public_id: string;
                    secret: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Complete single-source ICS representation */
                200: {
                    headers: {
                        "Cache-Control"?: "private, no-cache";
                        ETag?: string;
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Unchanged authorized representation; no response body */
                304: {
                    headers: {
                        "Cache-Control"?: "private, no-cache";
                        ETag?: string;
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Fixed error envelope; no calendar content or ETag */
                400: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Fixed error envelope; no calendar content or ETag */
                404: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Fixed error envelope; no calendar content or ETag */
                405: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Fixed error envelope; no calendar content or ETag */
                408: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Fixed error envelope; no calendar content or ETag */
                413: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Fixed error envelope; no calendar content or ETag */
                422: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Fixed error envelope; no calendar content or ETag */
                500: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Fixed error envelope; no calendar content or ETag */
                503: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
            };
        };
        patch?: never;
        trace?: never;
    };
    "/health/live": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Success */
                200: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @enum {string} */
                            status: "live";
                        };
                    };
                };
                /** @description Bounded error; request_id matches X-Request-Id */
                400: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["Error"];
                    };
                };
                /** @description Bounded error; request_id matches X-Request-Id */
                404: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["Error"];
                    };
                };
                /** @description Bounded error; request_id matches X-Request-Id */
                405: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["Error"];
                    };
                };
                /** @description Bounded error; request_id matches X-Request-Id */
                408: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["Error"];
                    };
                };
                /** @description Bounded error; request_id matches X-Request-Id */
                413: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["Error"];
                    };
                };
                /** @description Bounded error; request_id matches X-Request-Id */
                500: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["Error"];
                    };
                };
                /** @description Bounded error; request_id matches X-Request-Id */
                503: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["Error"];
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/health/ready": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Success */
                200: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @enum {string} */
                            status: "ready";
                        };
                    };
                };
                /** @description Bounded error; request_id matches X-Request-Id */
                400: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["Error"];
                    };
                };
                /** @description Bounded error; request_id matches X-Request-Id */
                404: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["Error"];
                    };
                };
                /** @description Bounded error; request_id matches X-Request-Id */
                405: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["Error"];
                    };
                };
                /** @description Bounded error; request_id matches X-Request-Id */
                408: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["Error"];
                    };
                };
                /** @description Bounded error; request_id matches X-Request-Id */
                413: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["Error"];
                    };
                };
                /** @description Bounded error; request_id matches X-Request-Id */
                500: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["Error"];
                    };
                };
                /** @description Bounded error; request_id matches X-Request-Id */
                503: {
                    headers: {
                        "X-Request-Id"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["Error"];
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
}
export type webhooks = Record<string, never>;
export interface components {
    schemas: {
        Error: {
            error: {
                code: string;
                request_id: string;
            };
        };
    };
    responses: never;
    parameters: never;
    requestBodies: never;
    headers: never;
    pathItems: never;
}
export type $defs = Record<string, never>;
export type operations = Record<string, never>;
