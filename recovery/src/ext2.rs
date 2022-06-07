// implementation template for convenience
// see https://www.nongnu.org/ext2-doc/ext2.html for documentation on ext2 fs

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::{io, mem};

const EXT2_SUPER_MAGIC: u16 = 0xEF53;
pub const JFIF_HEADER: &[u8] = b"\xFF\xD8\xFF\xE0\x00\x10\x4A\x46\x49\x46\x00";
pub const JFIF_EOI: &[u8] = b"\xFF\xD9";

#[derive(Debug)]
pub struct Ext2FS {
    pub device: File,
    pub superblock: Superblock,
    pub block_group_descriptor_tables: Vec<BlockGroupDescriptor>,
}

pub struct BlockIter<'a> {
    fs: &'a Ext2FS,
    block_id: usize,
    block_bitmap: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct Superblock {
    s_inodes_count: u32,      // the total number of inodes, both used and free
    s_blocks_count: u32, // the total number of blocks in the system including all used, free and reserved
    s_r_blocks_count: u32, // the total number of blocks reserved for the usage of the super user
    s_free_blocks_count: u32, // the total number of free blocks, including the number of reserved blocks
    s_free_inodes_count: u32, // the total number of free inodes
    s_first_data_block: u32,  // the id of the block containing the superblock structure
    s_log_block_size: u32,    // block size = 1024 << s_log_block_size
    s_log_frag_size: u32, // if( positive ) { fragment size = 1024 << s_log_frag_size } else { fragment size = 1024 >> -s_log_frag_size }
    s_blocks_per_group: u32, // the total number of blocks per group
    s_frags_per_group: u32, // the total number of fragments per group
    s_inodes_per_group: u32, // the total number of inodes per group
    s_mtime: u32,         // the last time the file system was mounted
    s_wtime: u32,         // the last write access to the file system
    s_mnt_count: u16, // how many time the file system was mounted since the last time it was fully verified
    s_max_mnt_count: u16, // the maximum number of times that the file system may be mounted before a full check is performed
    s_magic: u16, // identifying the file system as Ext2, it's fixed to EXT2_SUPER_MAGIC of value 0xEF53
    s_state: u16, // when the file system is mounted, this state is set to EXT2_ERROR_FS of value 2. After the file system was cleanly unmounted, this value is set to EXT2_VALID_FS of value 1.
    s_errors: u16, // indicating what the file system driver should do when an error is detected
    s_minor_rev_level: u16, // identifying the minor revision level within its revision level
    s_lastcheck: u32, // the time of the last file system check
    s_checkinterval: u32, // Maximum Unix time interval allowed between file system checks
    s_creator_os: u32, // the os that created the file system
    s_rev_level: u32, // revision level value
    s_def_resuid: u16, // the default user id for reserved blocks
    s_def_resgid: u16, // the default group id for reserved blocks
}

#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct BlockGroupDescriptor {
    bg_block_bitmap: u32, // block id of the first block of the “block bitmap” for the represented group
    bg_inode_bitmap: u32, // block id of the first block of the “inode bitmap” for the represented group
    bg_inode_table: u32, // block id of the first block of the “inode table” for the represented group
    bg_free_blocks_count: u16, // the total number of free blocks for the represented group
    bg_free_inodes_count: u16, // the total number of free inodes for the represented group
    bg_used_dirs_count: u16, // the number of inodes allocated to directories for the represented group
    bg_pad: u16,             // used for padding the structure on a 32bit boundary
    bg_reserved: [u8; 12],   // reserved space for future revisions
}

impl Superblock {
    pub fn from_file(mut file: &File) -> io::Result<Self> {
        file.seek(SeekFrom::Start(1024))?;
        let mut buf = [0u8; mem::size_of::<Self>()];
        file.read_exact(&mut buf)?;
        let superblock: Self = unsafe { mem::transmute(buf) };

        Ok(superblock)
    }

    pub fn is_ext2(&self) -> bool {
        self.s_magic == EXT2_SUPER_MAGIC
    }

    pub fn first_data_block(&self) -> usize {
        self.s_first_data_block as usize
    }

    pub fn block_size(&self) -> usize {
        1024 << self.s_log_block_size as usize
    }

    pub fn blocks_per_group(&self) -> usize {
        self.s_blocks_per_group as usize
    }

    pub fn block_count(&self) -> usize {
        self.s_blocks_count as usize
    }

    pub fn block_group_count(&self) -> usize {
        // s_blocks_per_group * block group count >= s_blocks_count due to volume size
        // to round up
        1 + (self.block_count() - 1) / self.blocks_per_group()
    }

    pub fn bitmap_size(&self) -> usize {
        1 + (self.blocks_per_group() - 1) / 8
    }
}

impl BlockGroupDescriptor {
    pub fn from_file(mut file: &File) -> io::Result<Self> {
        file.seek(SeekFrom::Start(2048))?;
        let mut buf = [0u8; mem::size_of::<Self>()];
        file.read_exact(&mut buf)?;
        let gdt: Self = unsafe { mem::transmute(buf) };

        Ok(gdt)
    }

    pub fn block_bitmap(&self) -> usize {
        self.bg_block_bitmap as usize
    }
}

impl Ext2FS {
    pub fn from_path(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = File::open(path)?;
        file.try_into()
    }

    pub fn read_block(&self, block_id: usize, buf: &mut Vec<u8>) -> io::Result<()> {
        let block_size = self.superblock.block_size();
        let offset = block_id * block_size;

        buf.resize(block_size, 0);
        (&self.device).seek(SeekFrom::Start(offset as u64))?;
        (&self.device).read_exact(buf)?;
        Ok(())
    }

    pub fn read_bitmap(&self, block_group: usize, buf: &mut Vec<u8>) -> io::Result<()> {
        let bitmap_size = self.superblock.bitmap_size();

        self.read_block(
            self.block_group_descriptor_tables[block_group].block_bitmap(),
            buf,
        )?;
        buf.truncate(bitmap_size);

        Ok(())
    }

    pub fn block_iter(&self) -> io::Result<BlockIter> {
        let bitmap_size = self.superblock.bitmap_size();
        let mut bitmap_block_data = Vec::with_capacity(self.superblock.block_size());

        self.read_bitmap(0, &mut bitmap_block_data)?;
        bitmap_block_data.truncate(bitmap_size);

        Ok(BlockIter {
            fs: self,
            block_id: 0,
            block_bitmap: bitmap_block_data,
        })
    }

    pub fn block_is_indirect(&self, block: usize) -> bool {
        let block_dist = self.superblock.block_size() / 4;

        assert!(
            {
                let max_blocks = 12 + block_dist + block_dist.pow(2) + block_dist.pow(3);
                let addr_blocks = 1 + 1 + block_dist + 1 + block_dist + block_dist * block_dist;
                block < max_blocks + addr_blocks
            },
            "over maximum block id"
        );

        if block < 12 {
            return false;
        }

        if block == 12 {
            return true;
        }

        let d_indirect = 13 + block_dist;
        let t_indirect = d_indirect + 1 + (block_dist + 1) * block_dist;

        // 2x
        if block == d_indirect {
            return true;
        }

        if block > d_indirect && block <= (d_indirect + 1) + (block_dist + 1) * (block_dist - 1) {
            return (block - (d_indirect + 1)) % (block_dist + 1) == 0;
        }

        // 3x
        if block == t_indirect {
            return true;
        }

        if block > t_indirect
            && block <= (t_indirect + 1) + (1 + block_dist * (block_dist + 1)) * (block_dist - 1)
        {
            // Treat the block after the triply-indirect block as a block_dist * (1 + (1 + block_dist) * block_dist) table.
            // every row contains a doubly-indirect block and block_dist * (one indirect block + block_dist * blocks)
            let blc = block - (t_indirect + 1);
            let r_size = 1 + (block_dist * (block_dist + 1));
            let row = blc / r_size;
            let col = blc - row * r_size;

            return col == 0 || col == 1 || (col - 1) % (block_dist + 1) == 0;
        }

        false
    }
}

impl TryFrom<File> for Ext2FS {
    type Error = io::Error;

    fn try_from(file: File) -> Result<Self, Self::Error> {
        let superblock = Superblock::from_file(&file)?;

        if !superblock.is_ext2() {
            return Err(io::ErrorKind::Unsupported.into());
        }

        let block_group_cnt = superblock.block_group_count();
        let mut gdts = Vec::with_capacity(block_group_cnt);
        for _ in 0..block_group_cnt {
            gdts.push(BlockGroupDescriptor::from_file(&file)?)
        }

        Ok(Self {
            device: file,
            superblock,
            block_group_descriptor_tables: gdts,
        })
    }
}

// iterate unused blocks
impl<'a> Iterator for BlockIter<'a> {
    type Item = usize;

    fn next(&mut self) -> Option<Self::Item> {
        let blocks_per_group = self.fs.superblock.blocks_per_group();
        let block_idx_in_group = self.block_id % blocks_per_group + 1;
        let mut group = self.block_id / blocks_per_group;
        let mut byte = block_idx_in_group / 8;
        let mut bit = block_idx_in_group % 8;

        loop {
            if byte >= self.block_bitmap.len() {
                group += 1;

                if group >= self.fs.superblock.block_group_count() {
                    return None;
                }

                // update block bitmap
                self.fs.read_bitmap(group, &mut self.block_bitmap).ok()?;

                byte = 0;
                bit = 0;
            }

            // set free as 1, used as 0
            let bits = !self.block_bitmap[byte] >> bit;
            // there are free blocks
            if bits != 0 {
                let first_free_block_offset = bits.trailing_zeros() as usize;
                bit += first_free_block_offset;
                self.block_id = group * blocks_per_group + byte * 8 + bit;
                break;
            } else {
                byte += 1;
                bit = 0;
            }
        }

        if self.block_id + self.fs.superblock.first_data_block() >= self.fs.superblock.block_count()
        {
            None
        } else {
            Some(self.block_id + self.fs.superblock.first_data_block())
        }
    }
}
