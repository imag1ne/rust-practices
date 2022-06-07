# File Recovery

In this task, we need to recover one or more deleted jpg files in the given file systems. 

The problem is that it is difficult to find the block in which a deleted file begins because modern Linux systems overwrite the inodes of the files when deleting them. The metadata of the deleted file is unknown, but its size of it must be determined. It is possible to locate the deleted jpg files by finding the header of its format.

In every provided file system there is at least one deleted jpg file, so we have to detect it, collect all related blocks of it, and then write it to a new file.
