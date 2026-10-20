//! Built-in tables of impact analysis: the packages that stop the reverse
//! walk, the compilers and build tools whose changes are full runs, the
//! standard-library modules built on the interpreter's native libraries, and
//! the import names of well-known distributions whose modules differ from
//! their name.

/// Pixi's default runtime barriers: the C/C++ runtimes nearly every native
/// package requires, whose changes would otherwise affect everything.
pub const RUNTIME_BARRIERS: &[&str] = &[
    "libgcc",
    "libgcc-ng",
    "libstdcxx",
    "libstdcxx-ng",
    "libgomp",
    "vc14_runtime",
    "vc",
    "ucrt",
    "vs2015_runtime",
    "libcxx",
    "_openmp_mutex",
    "msvc-runtime",
];

/// The conda interpreters. Each is always a barrier, and a change to one is
/// a full run.
pub const INTERPRETERS: &[&str] = &["python"];

/// Compilers and the tools that build native extensions, in any ecosystem:
/// a change to one can change every compiled extension, so it is a full run.
/// Conda's per-platform compiler packages (`gcc_linux-64`) match by the
/// prefixes in [`COMPILER_PREFIXES`].
pub const BUILD_TOOLS: &[&str] = &[
    "c-compiler",
    "cxx-compiler",
    "fortran-compiler",
    "compilers",
    "gcc",
    "gxx",
    "gfortran",
    "clang",
    "clangxx",
    "cmake",
    "ninja",
    "meson",
    "cython",
    "pybind11",
    "nanobind",
    "maturin",
    "scikit-build",
    "scikit-build-core",
    "meson-python",
    "setuptools-rust",
];

/// The name prefixes of conda's per-platform compiler packages, such as
/// `gcc_linux-64` or `clangxx_osx-arm64`.
pub const COMPILER_PREFIXES: &[&str] = &["gcc_", "gxx_", "gfortran_", "clang_", "clangxx_"];

/// Whether a change to the package `name` is a full run, as a compiler or a
/// build tool of native extensions.
pub(crate) fn is_build_tool(name: &str) -> bool {
    let name = name.to_ascii_lowercase().replace('_', "-");
    BUILD_TOOLS.contains(&name.as_str())
        || COMPILER_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(&prefix.replace('_', "-")))
}

/// The interpreter's native conda libraries, each with the Python
/// standard-library modules built on it.
pub const PYTHON_STDLIB_MAP: &[(&str, &[&str])] = &[
    ("openssl", &["ssl", "hashlib"]),
    ("libsqlite", &["sqlite3"]),
    ("libffi", &["ctypes"]),
    ("xz", &["lzma"]),
    ("liblzma", &["lzma"]),
    ("bzip2", &["bz2"]),
    ("zlib", &["zlib"]),
    ("libzlib", &["zlib"]),
    ("tk", &["tkinter"]),
    ("readline", &["readline"]),
    ("ncurses", &["curses"]),
    ("libexpat", &["xml.parsers.expat", "pyexpat"]),
    ("libuuid", &["uuid"]),
];

/// Well-known distributions whose modules differ from their name, PyPI and
/// conda names alike, each with the modules it provides. Names are matched
/// in normalized form; a namespace package names the module below it, such
/// as `google.protobuf`.
pub const PYTHON_IMPORT_NAMES: &[(&str, &[&str])] = &[
    ("attrs", &["attr", "attrs"]),
    ("azure-storage-blob", &["azure.storage.blob"]),
    ("beautifulsoup4", &["bs4"]),
    ("dnspython", &["dns"]),
    ("faiss-cpu", &["faiss"]),
    ("google-api-python-client", &["googleapiclient"]),
    ("google-cloud-storage", &["google.cloud.storage"]),
    ("grpcio", &["grpc"]),
    ("grpcio-tools", &["grpc_tools"]),
    ("ipython", &["IPython"]),
    ("matplotlib-base", &["matplotlib", "mpl_toolkits"]),
    ("msgpack-python", &["msgpack"]),
    ("mysqlclient", &["MySQLdb"]),
    ("opencv-contrib-python", &["cv2"]),
    ("opencv-python", &["cv2"]),
    ("opencv-python-headless", &["cv2"]),
    ("pillow", &["PIL"]),
    ("protobuf", &["google.protobuf"]),
    ("psycopg-binary", &["psycopg_binary"]),
    ("psycopg2-binary", &["psycopg2"]),
    ("py-opencv", &["cv2"]),
    ("py-xgboost", &["xgboost"]),
    ("pycryptodome", &["Crypto"]),
    ("pycryptodomex", &["Cryptodome"]),
    ("pygobject", &["gi"]),
    ("pyjwt", &["jwt"]),
    ("pymupdf", &["fitz", "pymupdf"]),
    ("pyopenssl", &["OpenSSL"]),
    ("pyqt5", &["PyQt5"]),
    ("pyserial", &["serial"]),
    ("pyside6", &["PySide6"]),
    ("python-dateutil", &["dateutil"]),
    ("python-dotenv", &["dotenv"]),
    ("python-jose", &["jose"]),
    ("python-magic", &["magic"]),
    ("python-multipart", &["multipart", "python_multipart"]),
    ("pytorch", &["torch"]),
    (
        "pywin32",
        &["win32api", "win32con", "pywintypes", "win32com"],
    ),
    ("pyyaml", &["yaml"]),
    ("pyzmq", &["zmq"]),
    ("ruamel-yaml", &["ruamel.yaml"]),
    ("scikit-image", &["skimage"]),
    ("scikit-learn", &["sklearn"]),
    (
        "setuptools",
        &["setuptools", "pkg_resources", "_distutils_hack"],
    ),
    ("tensorflow-cpu", &["tensorflow"]),
    ("websocket-client", &["websocket"]),
    ("zope-interface", &["zope.interface"]),
];
