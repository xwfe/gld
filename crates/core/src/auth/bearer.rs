/// 从 `Authorization` 头里取出 bearer token。
///
/// scheme 按大小写不敏感比对：RFC 7235 规定认证 scheme 不区分大小写，
/// RFC 6750 的 Bearer 也一样。写死 `strip_prefix("Bearer ")` 的话，
/// 发 `authorization: bearer xxx` 的客户端会一直拿 401，而 token 明明是对的
/// ——这种 401 最难查，因为看起来像 token 配错了。
pub fn bearer_token(header: &str) -> Option<&str> {
    let (scheme, token) = header.trim_start().split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = token.trim();
    (!token.is_empty()).then_some(token)
}

pub fn constant_time_eq_str(left: &str, right: &str) -> bool {
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
