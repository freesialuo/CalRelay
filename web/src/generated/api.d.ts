// Generated from the local Rust service OpenAPI. Run npm run generate.
export interface paths {
    "/api/admin/v1/outputs": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Discover at most 100 managed summaries including retired records */
        get: {
            parameters: {
                query?: {
                    after?: string;
                };
                header: {
                    "Cf-Access-Jwt-Assertion": string;
                };
                path?: never;
                cookie: {
                    "__Host-calrelay_admin": string;
                };
            };
            requestBody?: never;
            responses: {
                /** @description No-store page, binary id cursor; not a concurrent snapshot */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["ReminderConfigurationPage"];
                    };
                };
                /** @description Invalid query */
                400: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Denied */
                403: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Invalid cursor */
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
                /** @description Stopping */
                503: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
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
    "/api/admin/v1/outputs/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Managed outputs: conditional configuration; no secret values
         * @description Optional trusted admin assembly. Creation requires disabled configuration and If-None-Match: *. Updates/retirement require If-Match: quoted current generation. DELETE has an empty body. A consumed management nonce rotates even on downstream failure. Credential references must already exist with kind bark; no plaintext credential writes. All responses no-store.
         */
        get: {
            parameters: {
                query?: never;
                header: {
                    "Cf-Access-Jwt-Assertion": string;
                };
                path: {
                    id: string;
                };
                cookie: {
                    "__Host-calrelay_admin": string;
                };
            };
            requestBody?: never;
            responses: {
                /** @description Configuration receipt */
                200: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        ETag?: string;
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["BarkOutputConfigurationRecord"];
                    };
                };
                /** @description Bounded failure */
                400: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Management denied */
                403: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Unknown or unmanaged configuration */
                404: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Stale condition, existing, legacy or retired id */
                409: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Body budget exceeded */
                413: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Bounded failure */
                415: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Invalid configuration/reference/condition */
                422: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Bounded failure */
                500: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Stopping */
                503: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
            };
        };
        /**
         * Managed outputs: conditional configuration; no secret values
         * @description Optional trusted admin assembly. Creation requires disabled configuration and If-None-Match: *. Updates/retirement require If-Match: quoted current generation. DELETE has an empty body. A consumed management nonce rotates even on downstream failure. Credential references must already exist with kind bark; no plaintext credential writes. All responses no-store.
         */
        put: {
            parameters: {
                query?: never;
                header: {
                    "Cf-Access-Jwt-Assertion": string;
                    Origin: string;
                    "X-CSRF-Token": string;
                    "X-Admin-Write-Nonce": string;
                    "If-Match"?: string;
                    "If-None-Match"?: string;
                };
                path: {
                    id: string;
                };
                cookie: {
                    "__Host-calrelay_admin": string;
                };
            };
            requestBody: {
                content: {
                    "application/json": components["schemas"]["BarkOutputConfigurationV1"];
                };
            };
            responses: {
                /** @description Configuration receipt */
                200: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        ETag?: string;
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["BarkOutputConfigurationRecord"];
                    };
                };
                /** @description Configuration receipt */
                201: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        ETag?: string;
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["BarkOutputConfigurationRecord"];
                    };
                };
                /** @description Bounded failure */
                400: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Management denied */
                403: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Unknown or unmanaged configuration */
                404: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Stale condition, existing, legacy or retired id */
                409: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Body budget exceeded */
                413: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Bounded failure */
                415: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Invalid configuration/reference/condition */
                422: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Bounded failure */
                500: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Stopping */
                503: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
            };
        };
        post?: never;
        /**
         * Managed outputs: conditional configuration; no secret values
         * @description Optional trusted admin assembly. Creation requires disabled configuration and If-None-Match: *. Updates/retirement require If-Match: quoted current generation. DELETE has an empty body. A consumed management nonce rotates even on downstream failure. Credential references must already exist with kind bark; no plaintext credential writes. All responses no-store.
         */
        delete: {
            parameters: {
                query?: never;
                header: {
                    "Cf-Access-Jwt-Assertion": string;
                    Origin: string;
                    "X-CSRF-Token": string;
                    "X-Admin-Write-Nonce": string;
                    "If-Match": string;
                };
                path: {
                    id: string;
                };
                cookie: {
                    "__Host-calrelay_admin": string;
                };
            };
            requestBody?: never;
            responses: {
                /** @description Retired, history retained */
                204: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Bounded failure */
                400: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Management denied */
                403: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Unknown or unmanaged configuration */
                404: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Stale condition, existing, legacy or retired id */
                409: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Body budget exceeded */
                413: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Bounded failure */
                415: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Invalid configuration/reference/condition */
                422: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Bounded failure */
                500: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Stopping */
                503: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
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
    "/api/admin/v1/rules": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Discover at most 100 managed summaries including retired records */
        get: {
            parameters: {
                query?: {
                    after?: string;
                };
                header: {
                    "Cf-Access-Jwt-Assertion": string;
                };
                path?: never;
                cookie: {
                    "__Host-calrelay_admin": string;
                };
            };
            requestBody?: never;
            responses: {
                /** @description No-store page, binary id cursor; not a concurrent snapshot */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["ReminderConfigurationPage"];
                    };
                };
                /** @description Invalid query */
                400: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Denied */
                403: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Invalid cursor */
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
                /** @description Stopping */
                503: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
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
    "/api/admin/v1/rules/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Managed rules: conditional configuration; no secret values
         * @description Optional trusted admin assembly. Creation requires disabled configuration and If-None-Match: *. Updates/retirement require If-Match: quoted current generation. DELETE has an empty body. A consumed management nonce rotates even on downstream failure. Credential references must already exist with kind bark; no plaintext credential writes. All responses no-store.
         */
        get: {
            parameters: {
                query?: never;
                header: {
                    "Cf-Access-Jwt-Assertion": string;
                };
                path: {
                    id: string;
                };
                cookie: {
                    "__Host-calrelay_admin": string;
                };
            };
            requestBody?: never;
            responses: {
                /** @description Configuration receipt */
                200: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        ETag?: string;
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["RuleConfigurationRecord"];
                    };
                };
                /** @description Bounded failure */
                400: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Management denied */
                403: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Unknown or unmanaged configuration */
                404: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Stale condition, existing, legacy or retired id */
                409: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Body budget exceeded */
                413: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Bounded failure */
                415: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Invalid configuration/reference/condition */
                422: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Bounded failure */
                500: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Stopping */
                503: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
            };
        };
        /**
         * Managed rules: conditional configuration; no secret values
         * @description Optional trusted admin assembly. Creation requires disabled configuration and If-None-Match: *. Updates/retirement require If-Match: quoted current generation. DELETE has an empty body. A consumed management nonce rotates even on downstream failure. Credential references must already exist with kind bark; no plaintext credential writes. All responses no-store.
         */
        put: {
            parameters: {
                query?: never;
                header: {
                    "Cf-Access-Jwt-Assertion": string;
                    Origin: string;
                    "X-CSRF-Token": string;
                    "X-Admin-Write-Nonce": string;
                    "If-Match"?: string;
                    "If-None-Match"?: string;
                };
                path: {
                    id: string;
                };
                cookie: {
                    "__Host-calrelay_admin": string;
                };
            };
            requestBody: {
                content: {
                    "application/json": components["schemas"]["RuleConfigurationV1"];
                };
            };
            responses: {
                /** @description Configuration receipt */
                200: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        ETag?: string;
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["RuleConfigurationRecord"];
                    };
                };
                /** @description Configuration receipt */
                201: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        ETag?: string;
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["RuleConfigurationRecord"];
                    };
                };
                /** @description Bounded failure */
                400: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Management denied */
                403: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Unknown or unmanaged configuration */
                404: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Stale condition, existing, legacy or retired id */
                409: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Body budget exceeded */
                413: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Bounded failure */
                415: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Invalid configuration/reference/condition */
                422: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Bounded failure */
                500: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Stopping */
                503: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
            };
        };
        post?: never;
        /**
         * Managed rules: conditional configuration; no secret values
         * @description Optional trusted admin assembly. Creation requires disabled configuration and If-None-Match: *. Updates/retirement require If-Match: quoted current generation. DELETE has an empty body. A consumed management nonce rotates even on downstream failure. Credential references must already exist with kind bark; no plaintext credential writes. All responses no-store.
         */
        delete: {
            parameters: {
                query?: never;
                header: {
                    "Cf-Access-Jwt-Assertion": string;
                    Origin: string;
                    "X-CSRF-Token": string;
                    "X-Admin-Write-Nonce": string;
                    "If-Match": string;
                };
                path: {
                    id: string;
                };
                cookie: {
                    "__Host-calrelay_admin": string;
                };
            };
            requestBody?: never;
            responses: {
                /** @description Retired, history retained */
                204: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Bounded failure */
                400: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Management denied */
                403: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Unknown or unmanaged configuration */
                404: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Stale condition, existing, legacy or retired id */
                409: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Body budget exceeded */
                413: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Bounded failure */
                415: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Invalid configuration/reference/condition */
                422: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Bounded failure */
                500: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Stopping */
                503: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        "X-Admin-Write-Nonce"?: string;
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
    "/api/admin/v1/sources": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Discover managed sources including retired records; at most 100 summaries
         * @description Binary source_id cursor. Not snapshot pagination under concurrent writes. Unknown query fields rejected. No config bodies, tokens or credential handles.
         */
        get: {
            parameters: {
                query?: {
                    after?: string;
                };
                header: {
                    "Cf-Access-Jwt-Assertion": string;
                };
                path?: never;
                cookie: {
                    "__Host-calrelay_admin": string;
                };
            };
            requestBody?: never;
            responses: {
                /** @description Bounded no-store page */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["SourceConfigurationPage"];
                    };
                };
                /** @description Invalid query */
                400: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Management identity denied */
                403: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Invalid cursor */
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
                /** @description Stopping */
                503: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
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
    "/api/admin/v1/sources/{source_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Read, conditionally save, or retire source configuration (optional admin assembly)
         * @description GET/PUT ETag is config_revision, never event/source revision. PUT requires exactly one of If-None-Match: * (disabled creation) or If-Match: quoted positive config revision. DELETE requires If-Match and empty body. No secret material accepted. All responses no-store. Authorized unsafe requests consume and rotate the existing #34 nonce even on downstream failure; read X-Admin-Write-Nonce before retry.
         */
        get: {
            parameters: {
                query?: never;
                header: {
                    "Cf-Access-Jwt-Assertion": string;
                };
                path: {
                    source_id: string;
                };
                cookie: {
                    "__Host-calrelay_admin": string;
                };
            };
            requestBody?: never;
            responses: {
                /** @description Committed configuration receipt */
                200: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        ETag?: string;
                        /** @description Rotated after an authorized unsafe request */
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["SourceConfigurationRecord"];
                    };
                };
                /** @description Access/session/Origin/CSRF denied */
                403: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        /** @description Rotated after an authorized unsafe request */
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description No managed configuration; legacy sources are not reinterpreted */
                404: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        /** @description Rotated after an authorized unsafe request */
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Stale condition, identity conflict, existing or retired source */
                409: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        /** @description Rotated after an authorized unsafe request */
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Request budget exceeded */
                413: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        /** @description Rotated after an authorized unsafe request */
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Invalid configuration or condition */
                422: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        /** @description Rotated after an authorized unsafe request */
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Persistence failed */
                500: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        /** @description Rotated after an authorized unsafe request */
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Service stopping */
                503: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        /** @description Rotated after an authorized unsafe request */
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
            };
        };
        /**
         * Read, conditionally save, or retire source configuration (optional admin assembly)
         * @description GET/PUT ETag is config_revision, never event/source revision. PUT requires exactly one of If-None-Match: * (disabled creation) or If-Match: quoted positive config revision. DELETE requires If-Match and empty body. No secret material accepted. All responses no-store. Authorized unsafe requests consume and rotate the existing #34 nonce even on downstream failure; read X-Admin-Write-Nonce before retry.
         */
        put: {
            parameters: {
                query?: never;
                header: {
                    "Cf-Access-Jwt-Assertion": string;
                    Origin: string;
                    "X-CSRF-Token": string;
                    "X-Admin-Write-Nonce": string;
                    "If-Match"?: string;
                    "If-None-Match"?: string;
                };
                path: {
                    source_id: string;
                };
                cookie: {
                    "__Host-calrelay_admin": string;
                };
            };
            requestBody: {
                content: {
                    "application/json": components["schemas"]["SourceConfigurationV1"];
                };
            };
            responses: {
                /** @description Committed configuration receipt */
                200: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        ETag?: string;
                        /** @description Rotated after an authorized unsafe request */
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["SourceConfigurationRecord"];
                    };
                };
                /** @description Committed configuration receipt */
                201: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        ETag?: string;
                        /** @description Rotated after an authorized unsafe request */
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["SourceConfigurationRecord"];
                    };
                };
                /** @description Access/session/Origin/CSRF denied */
                403: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        /** @description Rotated after an authorized unsafe request */
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description No managed configuration; legacy sources are not reinterpreted */
                404: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        /** @description Rotated after an authorized unsafe request */
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Stale condition, identity conflict, existing or retired source */
                409: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        /** @description Rotated after an authorized unsafe request */
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Request budget exceeded */
                413: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        /** @description Rotated after an authorized unsafe request */
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Invalid configuration or condition */
                422: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        /** @description Rotated after an authorized unsafe request */
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Persistence failed */
                500: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        /** @description Rotated after an authorized unsafe request */
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Service stopping */
                503: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        /** @description Rotated after an authorized unsafe request */
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
            };
        };
        post?: never;
        /**
         * Read, conditionally save, or retire source configuration (optional admin assembly)
         * @description GET/PUT ETag is config_revision, never event/source revision. PUT requires exactly one of If-None-Match: * (disabled creation) or If-Match: quoted positive config revision. DELETE requires If-Match and empty body. No secret material accepted. All responses no-store. Authorized unsafe requests consume and rotate the existing #34 nonce even on downstream failure; read X-Admin-Write-Nonce before retry.
         */
        delete: {
            parameters: {
                query?: never;
                header: {
                    "Cf-Access-Jwt-Assertion": string;
                    Origin: string;
                    "X-CSRF-Token": string;
                    "X-Admin-Write-Nonce": string;
                    "If-Match": string;
                };
                path: {
                    source_id: string;
                };
                cookie: {
                    "__Host-calrelay_admin": string;
                };
            };
            requestBody?: never;
            responses: {
                /** @description Source retired; history retained */
                204: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        /** @description Rotated after an authorized unsafe request */
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Access/session/Origin/CSRF denied */
                403: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        /** @description Rotated after an authorized unsafe request */
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description No managed configuration; legacy sources are not reinterpreted */
                404: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        /** @description Rotated after an authorized unsafe request */
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Stale condition, identity conflict, existing or retired source */
                409: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        /** @description Rotated after an authorized unsafe request */
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Request budget exceeded */
                413: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        /** @description Rotated after an authorized unsafe request */
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Invalid configuration or condition */
                422: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        /** @description Rotated after an authorized unsafe request */
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Persistence failed */
                500: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        /** @description Rotated after an authorized unsafe request */
                        "X-Admin-Write-Nonce"?: string;
                        [name: string]: unknown;
                    };
                    content?: never;
                };
                /** @description Service stopping */
                503: {
                    headers: {
                        "Cache-Control"?: "no-store";
                        /** @description Rotated after an authorized unsafe request */
                        "X-Admin-Write-Nonce"?: string;
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
        BarkOutputConfigurationRecord: {
            configuration: components["schemas"]["BarkOutputConfigurationV1"];
            generation: number;
            output_id: string;
            retired_at: number | null;
        };
        /**
         * bark-output configuration v1
         * @description Strict management shape; application and transactional reference validation also required.
         */
        BarkOutputConfigurationV1: {
            credential_id: string;
            enabled: boolean;
            /** @constant */
            kind: "bark";
            name: string;
            /**
             * Format: uri
             * @description HTTPS origin or /push only; no credentials/query/fragment. Transport still enforces SSRF.
             */
            origin: string;
            request_timeout_ms: number;
            /** @constant */
            version: "v1";
        };
        Error: {
            error: {
                code: string;
                request_id: string;
            };
        };
        ReminderConfigurationPage: {
            items: components["schemas"]["ReminderConfigurationSummary"][];
            next_after: string | null;
        };
        ReminderConfigurationSummary: {
            enabled: boolean;
            generation: number;
            id: string;
            name: string;
            retired_at: number | null;
        };
        RuleConfigurationRecord: {
            configuration: components["schemas"]["RuleConfigurationV1"];
            generation: number;
            retired_at: number | null;
            rule_id: string;
        };
        /**
         * rule configuration v1
         * @description Strict management shape; application and transactional reference validation also required.
         */
        RuleConfigurationV1: {
            enabled: boolean;
            name: string;
            output_ids: string[];
            policy: {
                all_day_time: string;
                calendar_ids: string[];
                lead_ms: number;
            };
            /** @constant */
            version: "v1";
        };
        RulePolicy: {
            all_day_time: string;
            calendar_ids: string[];
            lead_ms: number;
        };
        SourceConfigurationPage: {
            items: components["schemas"]["SourceConfigurationSummary"][];
            next_after: string | null;
        };
        SourceConfigurationRecord: {
            config_revision: number;
            configuration: components["schemas"]["SourceConfigurationV1"];
            /** @description Unix milliseconds */
            retired_at: number | null;
            source_id: string;
            source_revision: number;
        };
        SourceConfigurationSummary: {
            config_revision: number;
            enabled: boolean;
            name: string;
            retired_at: number | null;
            source_id: string;
            source_revision: number;
        };
        /**
         * SourceConfigurationV1
         * @description Structural shape. Application validation additionally enforces capability/mode/URL/timezone/calendar identity semantics. See docs/source-configuration.md.
         */
        SourceConfigurationV1: {
            /** @enum {unknown} */
            adapter: "ics" | "standard-calendar";
            calendars: {
                external_id: string;
                id: string;
                selected: boolean;
                timezone?: string | null;
            }[];
            capabilities: {
                conditional_read: boolean;
                delete: boolean;
                delta: boolean;
                pagination: boolean;
                snapshot: boolean;
                upsert: boolean;
            };
            credential_id?: string | null;
            enabled: boolean;
            endpoint?: string | null;
            freshness_sla_ms?: number | null;
            /** @enum {unknown} */
            mode: "pull" | "manual-static" | "push-only";
            name: string;
            refresh_interval_ms?: number | null;
            /** @constant */
            version: "v1";
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
