use axum::http::{header::AUTHORIZATION, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

pub fn verify_bearer_header(headers: &HeaderMap, expected: &str) -> Option<Response> {
    // 期望值为空一律拒绝。空 token 配空期望值会 constant_time_eq 成功，
    // 也就是"配了 bearer 认证却谁都能进"。眼下 hyper 会把请求头行尾的空格
    // 裁掉，所以 `Bearer ` 到不了这儿——但那是别人家的实现细节，
    // 换个前置代理或换个 HTTP 栈就不成立了，认证不该靠这种巧合。
    if expected.is_empty() {
        return Some((StatusCode::UNAUTHORIZED, "Invalid bearer token").into_response());
    }

    let Some(header_value) = headers.get(AUTHORIZATION) else {
        return Some((StatusCode::UNAUTHORIZED, "Missing Authorization header").into_response());
    };

    let Ok(header_str) = header_value.to_str() else {
        return Some((StatusCode::UNAUTHORIZED, "Invalid Authorization header").into_response());
    };

    let Some(token) = bearer_token(header_str) else {
        return Some((StatusCode::UNAUTHORIZED, "Invalid bearer token").into_response());
    };

    if !constant_time_eq_str(token, expected) {
        return Some((StatusCode::UNAUTHORIZED, "Invalid bearer token").into_response());
    }

    None
}

/// 从 `Authorization` 头里取出 bearer token。
///
/// scheme 按大小写不敏感比对：RFC 7235 规定认证 scheme 不区分大小写，
/// RFC 6750 的 Bearer 也一样。写死 `strip_prefix("Bearer ")` 的话，
/// 发 `authorization: bearer xxx` 的客户端会一直拿 401，而 token 明明是对的
/// ——这种 401 最难查，因为看起来像 token 配错了。
pub(crate) fn bearer_token(header: &str) -> Option<&str> {
    let (scheme, token) = header.trim_start().split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = token.trim();
    (!token.is_empty()).then_some(token)
}

pub(crate) fn constant_time_eq_str(left: &str, right: &str) -> bool {
    constant_time_eq(left.as_bytes(), right.as_bytes())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right.iter())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_valid_bearer_token() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, "Bearer secret-token".parse().unwrap());
        assert!(verify_bearer_header(&headers, "secret-token").is_none());
    }

    #[test]
    fn rejects_missing_or_invalid_bearer_token() {
        let headers = HeaderMap::new();
        assert!(verify_bearer_header(&headers, "secret-token").is_some());

        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, "Basic secret-token".parse().unwrap());
        assert!(verify_bearer_header(&headers, "secret-token").is_some());

        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, "Bearer wrong".parse().unwrap());
        assert!(verify_bearer_header(&headers, "secret-token").is_some());
    }

    /// scheme 大小写不敏感（RFC 7235）。写死 "Bearer " 的话，
    /// 发小写的客户端会拿到 401，看起来像 token 配错了。
    #[test]
    fn the_scheme_is_case_insensitive() {
        for header in ["Bearer tok", "bearer tok", "BEARER tok", "BeArEr tok"] {
            let mut headers = HeaderMap::new();
            headers.insert(AUTHORIZATION, header.parse().unwrap());
            assert!(
                verify_bearer_header(&headers, "tok").is_none(),
                "{header} 应当通过"
            );
        }
    }

    /// 期望值为空时必须拒绝，否则"配了认证"等于"谁都能进"。
    #[test]
    fn an_empty_expected_token_never_authenticates() {
        for header in ["Bearer ", "Bearer", "bearer  ", "Bearer x"] {
            let mut headers = HeaderMap::new();
            // 行尾空格进不了 HeaderValue 的话就跳过——这里要证的是
            // 即便进来了也过不去。
            let Ok(value) = header.parse() else { continue };
            headers.insert(AUTHORIZATION, value);
            assert!(
                verify_bearer_header(&headers, "").is_some(),
                "expected 为空时 {header:?} 不该通过"
            );
        }
        // 直接打函数，绕开 HeaderValue 对行尾空格的裁剪。
        assert_eq!(bearer_token("Bearer "), None);
        assert_eq!(bearer_token("Bearer   "), None);
        assert_eq!(bearer_token("Bearer ok"), Some("ok"));
    }
}
