use url::Url;

pub struct S3Config {
    pub endpoint: String,
    pub project_ref: String,
}

pub fn parse_s3_config(supabase_endpoint: &str) -> anyhow::Result<S3Config> {
    let parsed_url = Url::parse(supabase_endpoint)?;
    let host_str = parsed_url.host_str().ok_or_else(|| anyhow::anyhow!("SUPABASE_ENDPOINT missing host"))?;
    
    if host_str == "127.0.0.1" || host_str == "localhost" {
        let clean_endpoint = supabase_endpoint.trim_end_matches('/');
        Ok(S3Config {
            endpoint: format!("{}/storage/v1/s3/", clean_endpoint),
            project_ref: "local-stub".to_string(),
        })
    } else {
        // Assume standard supabase URL format: https://<project_ref>.supabase.co
        let project_ref = host_str.split('.').next().ok_or_else(|| anyhow::anyhow!("Invalid project ref format"))?.to_string();
        Ok(S3Config {
            endpoint: format!("https://{}.supabase.co/storage/v1/s3/", project_ref),
            project_ref,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_s3_config_cloud() {
        let config = parse_s3_config("https://pkckwgszwgrvxwwdofcj.supabase.co").unwrap();
        assert_eq!(config.project_ref, "pkckwgszwgrvxwwdofcj");
        assert_eq!(config.endpoint, "https://pkckwgszwgrvxwwdofcj.supabase.co/storage/v1/s3/");
    }

    #[test]
    fn test_parse_s3_config_local() {
        let config = parse_s3_config("http://localhost:54321").unwrap();
        assert_eq!(config.project_ref, "local-stub");
        assert_eq!(config.endpoint, "http://localhost:54321/storage/v1/s3/");
    }

    #[test]
    fn test_parse_s3_config_invalid() {
        let res = parse_s3_config("not-a-url");
        assert!(res.is_err());
    }
}
