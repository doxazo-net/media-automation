// `*_STEMS` are matched as substrings (word.contains(stem)); `*_EXACT` is
// whole-word (\bword\b). `cum`/`cock`/`pussy` live as EXACT, not stems: as
// substrings they flag innocent words (scum, tecum, cumberland, circumvent,
// peacock, pussycat, cockapoo, and Latin "tecum/cum" in the Ave Maria). Common
// genuine inflections are kept as exact entries so true hits aren't lost.
pub const R_STEMS: &[&str] = &["fuck", "shit", "faggot"];
pub const R_EXACT: &[&str] = &[
    "blowjob",
    "cocksucker",
    "motherfuck",
    "bullshit",
    "cum",
    "cumming",
    "cums",
    "cock",
    "cocks",
    "pussy",
    "pussies",
];
pub const PG13_STEMS: &[&str] = &["bitch", "whore", "slut"];
pub const PG13_EXACT: &[&str] = &["hoe", "asshole", "piss"];
pub const FALSE_POSITIVES: &[&str] = &[
    "cockatoo",
    "cockatiel",
    "cocktail",
    "hancock",
    "dickens",
    "dickson",
    "scunthorpe",
    "pissarro",
    "circumstan",
    "cucumber",
    "cumulative",
    "cumbersome",
    "cumberbatch",
    "document",
    "incumbent",
    "succumb",
    "accumulate",
    "shiitake",
    "shitake",
];

pub const DEFAULT_G_GENRES: &[&str] = &[
    "Ambient",
    "Classical",
    "Instrumental",
    "Meditation",
    "New Age",
    "Orchestral",
    "Piano",
];
