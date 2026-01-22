/// 根据文件后缀返回 Content-Type
pub fn get_mime_type(filename: &str) -> &str {
    if filename.ends_with(".html") { "text/html" }
    else if filename.ends_with(".css") { "text/css" }
    else if filename.ends_with(".js") { "application/javascript" }
    else if filename.ends_with(".png") { "image/png" }
    else if filename.ends_with(".jpg") || filename.ends_with(".jpeg") { "image/jpeg" }
    else { "application/octet-stream" }
}
