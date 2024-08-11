use teloxide::types::InputFile;

use crate::config::EspCam;

pub async fn read_camera_image(
    client: reqwest::Client,
    camera_config: &EspCam,
) -> anyhow::Result<InputFile> {
    let response = client.get(camera_config.url.clone()).send().await?;
    let image_bytes = response.bytes().await?;
    let input_file = InputFile::memory(image_bytes);
    Ok(input_file)
}
