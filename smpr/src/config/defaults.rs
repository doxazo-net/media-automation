// `*_STEMS` are matched as substrings (word.contains(stem)); `*_EXACT` is
// whole-word (\bword\b). `cum`/`cock`/`pussy` live as EXACT, not stems: as
// substrings they flag innocent words (scum, tecum, cumberland, circumvent,
// peacock, pussycat, cockapoo, and Latin "tecum/cum" in the Ave Maria). Common
// genuine inflections are kept as exact entries so true hits aren't lost.
//
// The exact-vs-stem choice is asymmetric by compound productivity:
//   - `shit` stays a STEM. It is highly productive in English compounds
//     (dipshit, batshit, horseshit, chickenshit, shitshow, shitload, shittin,
//     shite, ...); making it exact would silently miss any compound not
//     enumerated, and a missed profanity is the worse failure for a parental
//     tool than a rare false positive. Its few substring collisions on
//     non-English tokens (romaji `-shite` te-forms like kaoshite/shitemo/
//     haidashite, and crashity) are suppressed via FALSE_POSITIVES instead.
//   - `slut` is EXACT. It has almost no productive compounds, and the sole
//     observed collision (`beslut`, Scandinavian for "decision") is eliminated
//     structurally by the word boundary, so exact is safe here.
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
pub const PG13_STEMS: &[&str] = &["bitch", "whore"];
pub const PG13_EXACT: &[&str] = &["hoe", "asshole", "piss", "slut", "sluts", "slutty"];
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
    // Non-English substring collisions on the `shit` stem (kept as a stem for
    // compound coverage; see the header comment). The Japanese romaji te-/ta-form
    // family (`-shite`/`-shita`) is open-ended, so this is targeted suppression of
    // the tokens observed on a real ~82k-track library, not an exhaustive rule.
    "kaoshite",
    "shitemo",
    "haidashite",
    "crashity",
    "shita",   // covers "shita" (した) and "ashita" (明日)
    "dashite", // e.g. "hikidashite", "dashite"
    "furishite",
    "watashitachi", // 私たち ("we")
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
