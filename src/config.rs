// NOT secrets. Safe to ship in the binary.
pub const GH_CLIENT_ID: &str = "Iv23li38CdqDEPP71wWp";   // from your App settings
pub const GH_APP_ID: i64 = 1768239;            // App ID (integer)
pub const GH_APP_SLUG: &str = "starthub-cli";   // App slug

// StarHub API configuration
pub const STARTHUB_API_BASE: &str = "https://api.starthub.so";
// Note: This should be a service role key (JWT), not a publishable key
// Get this from your Supabase dashboard: Settings > API > service_role key
pub const STARTHUB_API_KEY: &str = "sb_publishable_AKGy20M54_uMOdJme3ZnZA_GX11LgHe";

// S3 Storage credentials for Supabase Storage S3 compatibility
pub const S3_ACCESS_KEY: &str = "68ce4ca3c9283491a4242363d928ea38";
pub const S3_SECRET_KEY: &str = "d47477e11d369cb3d2d3da2ff24de064a105276cc670e6bbc565f3ba283a902c";

// Supabase Storage S3 endpoint from your project dashboard
pub const SUPABASE_STORAGE_S3_ENDPOINT: &str = "https://smltnjrrzkmazvbrqbkq.storage.supabase.co/storage/v1/s3";
pub const SUPABASE_STORAGE_REGION: &str = "eu-central-1";
