// JSDoc mirrors of the server's JSON (docs/API.md; android/.../api/Models.kt). Every field may be
// missing in an older server's answer: readers use `?? default`. Types only, no code.

/**
 * @typedef {object} Health
 * @property {string} [status]
 * @property {string} [version]
 * @property {boolean} [db]
 * @property {string[]} [features]
 */

/**
 * @typedef {object} ReleaseAttrs
 * @property {string | null} [resolution]
 * @property {string | null} [source]
 * @property {string | null} [video_codec]
 * @property {string[]} [hdr]
 * @property {number | null} [bit_depth]
 * @property {string[]} [audio]
 * @property {string | null} [channels]
 * @property {string[]} [languages]
 * @property {string[]} [subtitles]
 * @property {string[]} [french]
 * @property {string | null} [group]
 * @property {number | null} [season]
 * @property {number | null} [episode]
 * @property {boolean} [complete]
 * @property {number | null} [year]
 * @property {string[]} [editions]
 * @property {string | null} [bitrate]
 * @property {string | null} [sample_rate]
 * @property {string[]} [formats]
 */

/**
 * @typedef {object} TorrentSummary
 * @property {string} infohash
 * @property {string} [title]
 * @property {string} [category]
 * @property {number | null} [total_size]
 * @property {number} [file_count]
 * @property {number} [created_at]
 * @property {string} [pubkey]
 * @property {string} [npub]
 * @property {number | null} [seeders]
 * @property {number | null} [leechers]
 * @property {boolean | null} [dead]
 * @property {number} [trust]
 * @property {number} [publisher_count]
 * @property {string | null} [media_title]
 * @property {number | null} [media_year]
 * @property {string | null} [media_kind]
 * @property {string | null} [matched_file]
 * @property {number | null} [media_id]
 * @property {number | null} [media_rating]
 * @property {ReleaseAttrs | null} [attrs]
 * @property {string} [origin]
 * @property {number | null} [best_peers]
 */

/**
 * @typedef {object} SearchPage
 * @property {TorrentSummary[]} [hits]
 * @property {number} [total]
 * @property {string} [source]
 * @property {boolean} [total_is_lower_bound]
 */

/**
 * @typedef {object} FacetCount
 * @property {string} value
 * @property {number} count
 */

/**
 * @typedef {object} FacetCounts
 * @property {string} field
 * @property {number} [total]
 * @property {FacetCount[]} [counts]
 */

/**
 * @typedef {object} FacetSet
 * @property {number} [total]
 * @property {FacetCounts[]} [facets]
 */

/**
 * @typedef {object} MediaInfo
 * @property {number} media_id
 * @property {string} [kind]
 * @property {string} [title]
 * @property {string | null} [original_title]
 * @property {number | null} [year]
 * @property {number | null} [end_year]
 * @property {string[]} [genres]
 * @property {number | null} [rating]
 * @property {number | null} [votes]
 * @property {number | null} [runtime_min]
 * @property {number | null} [season]
 * @property {number | null} [episode]
 * @property {string | null} [imdb]
 * @property {string | null} [overview]
 * @property {string | null} [poster]
 * @property {string | null} [episode_title]
 */

/**
 * @typedef {object} ReleaseGroup
 * @property {number | null} [season]
 * @property {number | null} [episode]
 * @property {string | null} [resolution]
 * @property {string} [language]
 * @property {string} [language_label]
 * @property {TorrentSummary[]} [releases]
 */

/**
 * @typedef {object} MediaReleases
 * @property {MediaInfo | null} [media]
 * @property {number} [total]
 * @property {boolean} [truncated]
 * @property {ReleaseGroup[]} [groups]
 */

/**
 * @typedef {object} TrustSettings
 * @property {string | null} [root]
 * @property {number} [threshold]
 */

/**
 * @typedef {object} SavedSearch
 * @property {number} id
 * @property {string} [name]
 * @property {Record<string, unknown>} [query]
 * @property {boolean} [notify]
 * @property {number | null} [last_checked]
 * @property {number | null} [last_seen_created_at]
 */

/**
 * `GET /api/v1/web/session` and the login answer.
 * @typedef {object} WebSession
 * @property {string} csrf
 * @property {number} [expires_at]
 * @property {number} [created_at]
 * @property {boolean} [ntfy]
 */

/**
 * What every view gets (docs/WEB.md "Module map").
 * @typedef {object} Ctx
 * @property {import("./api.js").Api} api
 * @property {import("./store.js").AppStore} store
 * @property {import("./health.js").HealthState} health
 * @property {typeof import("./prefs.js")} prefs
 * @property {(to: string, opts?: {replace?: boolean}) => void} nav
 * @property {() => void} back
 * @property {import("./ui/snackbar.js").Snackbar} snackbar
 * @property {typeof import("./ui/dialog.js")} dialog
 * @property {"phone" | "medium" | "wide"} layout
 * @property {import("./session.js").Session} session
 * @property {import("./pagecache.js").PageCache} cache
 * @property {boolean} [pane] true when a view is mounted inside the search workspace's detail pane
 */

/**
 * @typedef {object} ViewHandle
 * @property {() => void} unmount
 * @property {(params: Record<string, string>, query: URLSearchParams) => void} [update]
 */

/**
 * @typedef {object} ViewArgs
 * @property {Record<string, string>} params
 * @property {URLSearchParams} query
 * @property {Ctx} ctx
 */

export {};
