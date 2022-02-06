// This file is part of the uutils coreutils package.
//
// (c) Fangxu Hu <framlog@gmail.com>
// (c) Sylvestre Ledru <sylvestre@debian.org>
//
// For the full copyright and license information, please view the LICENSE file
// that was distributed with this source code.
mod table;

use uucore::error::UResult;
#[cfg(unix)]
use uucore::fsext::statfs_fn;
use uucore::fsext::{read_fs_list, FsUsage, MountInfo};

use clap::{crate_version, App, AppSettings, Arg, ArgMatches};

use std::cell::Cell;
use std::collections::HashMap;
use std::collections::HashSet;
#[cfg(unix)]
use std::ffi::CString;
use std::iter::FromIterator;
#[cfg(unix)]
use std::mem;

#[cfg(windows)]
use std::path::Path;

use crate::table::{DisplayRow, Header, Row};

static ABOUT: &str = "Show information about the file system on which each FILE resides,\n\
                      or all file systems by default.";

static OPT_ALL: &str = "all";
static OPT_BLOCKSIZE: &str = "blocksize";
static OPT_DIRECT: &str = "direct";
static OPT_TOTAL: &str = "total";
static OPT_HUMAN_READABLE: &str = "human-readable";
static OPT_HUMAN_READABLE_2: &str = "human-readable-2";
static OPT_INODES: &str = "inodes";
static OPT_KILO: &str = "kilo";
static OPT_LOCAL: &str = "local";
static OPT_NO_SYNC: &str = "no-sync";
static OPT_OUTPUT: &str = "output";
static OPT_PATHS: &str = "paths";
static OPT_PORTABILITY: &str = "portability";
static OPT_SYNC: &str = "sync";
static OPT_TYPE: &str = "type";
static OPT_PRINT_TYPE: &str = "print-type";
static OPT_EXCLUDE_TYPE: &str = "exclude-type";

/// Store names of file systems as a selector.
/// Note: `exclude` takes priority over `include`.
#[derive(Default)]
struct FsSelector {
    include: HashSet<String>,
    exclude: HashSet<String>,
}

#[derive(Default)]
struct Options {
    show_local_fs: bool,
    show_all_fs: bool,
    show_listed_fs: bool,
    show_fs_type: bool,
    show_inode_instead: bool,
    // block_size: usize,
    human_readable_base: i64,
    fs_selector: FsSelector,
}

impl Options {
    /// Convert command-line arguments into [`Options`].
    fn from(matches: &ArgMatches) -> Self {
        Self {
            show_local_fs: matches.is_present(OPT_LOCAL),
            show_all_fs: matches.is_present(OPT_ALL),
            show_listed_fs: false,
            show_fs_type: matches.is_present(OPT_PRINT_TYPE),
            show_inode_instead: matches.is_present(OPT_INODES),
            human_readable_base: if matches.is_present(OPT_HUMAN_READABLE) {
                1024
            } else if matches.is_present(OPT_HUMAN_READABLE_2) {
                1000
            } else {
                -1
            },
            fs_selector: FsSelector::from(matches),
        }
    }
}

#[derive(Debug, Clone)]
struct Filesystem {
    mount_info: MountInfo,
    usage: FsUsage,
}

fn usage() -> String {
    format!("{0} [OPTION]... [FILE]...", uucore::execution_phrase())
}

impl FsSelector {
    /// Convert command-line arguments into a [`FsSelector`].
    ///
    /// This function reads the include and exclude sets from
    /// [`ArgMatches`] and returns the corresponding [`FsSelector`]
    /// instance.
    fn from(matches: &ArgMatches) -> Self {
        let include = HashSet::from_iter(matches.values_of_lossy(OPT_TYPE).unwrap_or_default());
        let exclude = HashSet::from_iter(
            matches
                .values_of_lossy(OPT_EXCLUDE_TYPE)
                .unwrap_or_default(),
        );
        Self { include, exclude }
    }

    fn should_select(&self, fs_type: &str) -> bool {
        if self.exclude.contains(fs_type) {
            return false;
        }
        self.include.is_empty() || self.include.contains(fs_type)
    }
}

impl Filesystem {
    // TODO: resolve uuid in `mount_info.dev_name` if exists
    fn new(mount_info: MountInfo) -> Option<Self> {
        let _stat_path = if !mount_info.mount_dir.is_empty() {
            mount_info.mount_dir.clone()
        } else {
            #[cfg(unix)]
            {
                mount_info.dev_name.clone()
            }
            #[cfg(windows)]
            {
                // On windows, we expect the volume id
                mount_info.dev_id.clone()
            }
        };
        #[cfg(unix)]
        unsafe {
            let path = CString::new(_stat_path).unwrap();
            let mut statvfs = mem::zeroed();
            if statfs_fn(path.as_ptr(), &mut statvfs) < 0 {
                None
            } else {
                Some(Self {
                    mount_info,
                    usage: FsUsage::new(statvfs),
                })
            }
        }
        #[cfg(windows)]
        Some(Self {
            mount_info,
            usage: FsUsage::new(Path::new(&_stat_path)),
        })
    }
}

fn filter_mount_list(vmi: Vec<MountInfo>, paths: &[String], opt: &Options) -> Vec<MountInfo> {
    vmi.into_iter()
        .filter_map(|mi| {
            if (mi.remote && opt.show_local_fs)
                || (mi.dummy && !opt.show_all_fs && !opt.show_listed_fs)
                || !opt.fs_selector.should_select(&mi.fs_type)
            {
                None
            } else {
                if paths.is_empty() {
                    // No path specified
                    return Some((mi.dev_id.clone(), mi));
                }
                if paths.contains(&mi.mount_dir) {
                    // One or more paths have been provided
                    Some((mi.dev_id.clone(), mi))
                } else {
                    // Not a path we want to see
                    None
                }
            }
        })
        .fold(
            HashMap::<String, Cell<MountInfo>>::new(),
            |mut acc, (id, mi)| {
                #[allow(clippy::map_entry)]
                {
                    if acc.contains_key(&id) {
                        let seen = acc[&id].replace(mi.clone());
                        let target_nearer_root = seen.mount_dir.len() > mi.mount_dir.len();
                        // With bind mounts, prefer items nearer the root of the source
                        let source_below_root = !seen.mount_root.is_empty()
                            && !mi.mount_root.is_empty()
                            && seen.mount_root.len() < mi.mount_root.len();
                        // let "real" devices with '/' in the name win.
                        if (!mi.dev_name.starts_with('/') || seen.dev_name.starts_with('/'))
                            // let points towards the root of the device win.
                            && (!target_nearer_root || source_below_root)
                            // let an entry over-mounted on a new device win...
                            && (seen.dev_name == mi.dev_name
                            /* ... but only when matching an existing mnt point,
                            to avoid problematic replacement when given
                            inaccurate mount lists, seen with some chroot
                            environments for example.  */
                            || seen.mount_dir != mi.mount_dir)
                        {
                            acc[&id].replace(seen);
                        }
                    } else {
                        acc.insert(id, Cell::new(mi));
                    }
                    acc
                }
            },
        )
        .into_iter()
        .map(|ent| ent.1.into_inner())
        .collect::<Vec<_>>()
}

#[uucore::main]
pub fn uumain(args: impl uucore::Args) -> UResult<()> {
    let usage = usage();
    let matches = uu_app().override_usage(&usage[..]).get_matches_from(args);

    let paths: Vec<String> = matches
        .values_of(OPT_PATHS)
        .map(|v| v.map(ToString::to_string).collect())
        .unwrap_or_default();

    #[cfg(windows)]
    {
        if matches.is_present(OPT_INODES) {
            println!("{}: doesn't support -i option", uucore::util_name());
            return Ok(());
        }
    }

    let opt = Options::from(&matches);

    let mounts = read_fs_list();
    let data: Vec<Row> = filter_mount_list(mounts, &paths, &opt)
        .into_iter()
        .filter_map(Filesystem::new)
        .filter(|fs| fs.usage.blocks != 0 || opt.show_all_fs || opt.show_listed_fs)
        .map(Into::into)
        .collect();
    println!("{}", Header::new(&opt));
    for row in data {
        println!("{}", DisplayRow::new(row, &opt));
    }

    Ok(())
}

pub fn uu_app<'a>() -> App<'a> {
    App::new(uucore::util_name())
        .version(crate_version!())
        .about(ABOUT)
        .setting(AppSettings::InferLongArgs)
        .arg(
            Arg::new(OPT_ALL)
                .short('a')
                .long("all")
                .help("include dummy file systems"),
        )
        .arg(
            Arg::new(OPT_BLOCKSIZE)
                .short('B')
                .long("block-size")
                .takes_value(true)
                .help(
                    "scale sizes by SIZE before printing them; e.g.\
                     '-BM' prints sizes in units of 1,048,576 bytes",
                ),
        )
        .arg(
            Arg::new(OPT_DIRECT)
                .long("direct")
                .help("show statistics for a file instead of mount point"),
        )
        .arg(
            Arg::new(OPT_TOTAL)
                .long("total")
                .help("produce a grand total"),
        )
        .arg(
            Arg::new(OPT_HUMAN_READABLE)
                .short('h')
                .long("human-readable")
                .conflicts_with(OPT_HUMAN_READABLE_2)
                .help("print sizes in human readable format (e.g., 1K 234M 2G)"),
        )
        .arg(
            Arg::new(OPT_HUMAN_READABLE_2)
                .short('H')
                .long("si")
                .conflicts_with(OPT_HUMAN_READABLE)
                .help("likewise, but use powers of 1000 not 1024"),
        )
        .arg(
            Arg::new(OPT_INODES)
                .short('i')
                .long("inodes")
                .help("list inode information instead of block usage"),
        )
        .arg(Arg::new(OPT_KILO).short('k').help("like --block-size=1K"))
        .arg(
            Arg::new(OPT_LOCAL)
                .short('l')
                .long("local")
                .help("limit listing to local file systems"),
        )
        .arg(
            Arg::new(OPT_NO_SYNC)
                .long("no-sync")
                .conflicts_with(OPT_SYNC)
                .help("do not invoke sync before getting usage info (default)"),
        )
        .arg(
            Arg::new(OPT_OUTPUT)
                .long("output")
                .takes_value(true)
                .use_delimiter(true)
                .help(
                    "use the output format defined by FIELD_LIST,\
                     or print all fields if FIELD_LIST is omitted.",
                ),
        )
        .arg(
            Arg::new(OPT_PORTABILITY)
                .short('P')
                .long("portability")
                .help("use the POSIX output format"),
        )
        .arg(
            Arg::new(OPT_SYNC)
                .long("sync")
                .conflicts_with(OPT_NO_SYNC)
                .help("invoke sync before getting usage info"),
        )
        .arg(
            Arg::new(OPT_TYPE)
                .short('t')
                .long("type")
                .allow_invalid_utf8(true)
                .takes_value(true)
                .use_delimiter(true)
                .help("limit listing to file systems of type TYPE"),
        )
        .arg(
            Arg::new(OPT_PRINT_TYPE)
                .short('T')
                .long("print-type")
                .help("print file system type"),
        )
        .arg(
            Arg::new(OPT_EXCLUDE_TYPE)
                .short('x')
                .long("exclude-type")
                .takes_value(true)
                .use_delimiter(true)
                .help("limit listing to file systems not of type TYPE"),
        )
        .arg(Arg::new(OPT_PATHS).multiple_occurrences(true))
}
