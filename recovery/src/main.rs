use crate::ext2::{Ext2FS, JFIF_EOI, JFIF_HEADER};
use std::io::Write;
use std::{fs, io};

mod ext2;

// read superblock, iterate over unused blocks, find jpg start and end patterns, copy data
fn recover_files(device: fs::File, target_path: &str) -> io::Result<()> {
    let ext2fs = Ext2FS::try_from(device)?;
    let mut block_iter = ext2fs.block_iter()?;
    let mut block_data = Vec::new();
    let mut imgs = Vec::new();

    while let Some(block_id) = block_iter.next() {
        ext2fs.read_block(block_id, &mut block_data)?;
        let mut byte_iter = block_data.iter();
        if byte_iter.all(|&byte| byte == 0) {
            continue;
        }

        if let Some(start) = pattern_position(&block_data, JFIF_HEADER, 0) {
            let mut block_cnt = 0;
            if let Some(end) = pattern_position(&block_data, JFIF_EOI, start) {
                imgs.push(block_data[start..end + JFIF_EOI.len()].to_vec());
            } else {
                let mut img = Vec::new();
                img.extend_from_slice(&block_data[start..]);

                while let Some(block_id) = block_iter.next() {
                    ext2fs.read_block(block_id, &mut block_data)?;
                    block_cnt += 1;
                    // skip indirect blocks
                    if ext2fs.block_is_indirect(block_cnt) {
                        continue;
                    }
                    if let Some(end) = pattern_position(&block_data, JFIF_EOI, 0) {
                        img.extend_from_slice(&block_data[..end + JFIF_EOI.len()]);
                        imgs.push(img);
                        break;
                    } else {
                        img.extend_from_slice(&block_data);
                    }
                }
            }
        }
    }

    for (i, img) in imgs.iter().enumerate() {
        let mut file = fs::File::create(format!("{}/{}.jpg", target_path, i))?;
        file.write_all(img)?;
    }

    Ok(())
}

fn pattern_position(slice: &[u8], pattern: &[u8], offset: usize) -> Option<usize> {
    slice
        .windows(pattern.len())
        .skip(offset)
        .position(|data| data == pattern)
}

fn main() -> io::Result<()> {
    use std::env::args;
    let device_path = args().nth(1).unwrap_or("./examples/small.img".to_string());
    let path = std::path::PathBuf::from(device_path.clone());
    let suffix = path
        .file_name()
        .unwrap_or_default()
        .to_str()
        .unwrap_or_default();
    let target_path = args().nth(2).unwrap_or(format!("restored_{}", suffix));

    fs::create_dir_all(&target_path)?;
    recover_files(fs::File::open(&device_path)?, &target_path)?;

    Ok(())
}
