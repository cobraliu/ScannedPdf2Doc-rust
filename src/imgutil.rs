//! 灰度图上的几个位运算 —— 这些正是 Python 版里 OpenCV 干的活
//!
//! Python 版为了 `morphologyEx` + `connectedComponentsWithStats` 两个函数拖进
//! 整个 opencv-python(118 MB, 里面 78 MB 是 ffmpeg/x265/aom 这些视频编解码)。
//! 我们只需要这两件事, 而且核是 1xk / kx1 这种退化形状, 直接按行程写反而更快:
//! 通用形态学要 O(W*H*k), 行程法 O(W*H)。

/// 8 位灰度图
pub struct Gray {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

impl Gray {
    #[inline]
    pub fn at(&self, x: usize, y: usize) -> u8 {
        self.px[y * self.w + x]
    }

    /// 小于阈值(即"墨迹")的置 1
    pub fn binarize(&self, dark: u8) -> Bitmap {
        Bitmap {
            w: self.w,
            h: self.h,
            px: self.px.iter().map(|&v| u8::from(v < dark)).collect(),
        }
    }
}

/// 0/1 二值图
#[derive(Clone)]
pub struct Bitmap {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

impl Bitmap {
    pub fn new(w: usize, h: usize) -> Self {
        Self {
            w,
            h,
            px: vec![0; w * h],
        }
    }

    #[inline]
    pub fn at(&self, x: usize, y: usize) -> u8 {
        self.px[y * self.w + x]
    }

    #[inline]
    pub fn set(&mut self, x: usize, y: usize, v: u8) {
        self.px[y * self.w + x] = v;
    }

    /// 用 1xk (horiz) 或 kx1 (!horiz) 的核做开运算
    ///
    /// 开 = 先腐蚀再膨胀, 对这种一维核就等价于"只留下长度 >= k 的连续行程"。
    /// 汉字的横竖笔画再粗也短, 一开就没了; 留下来的才是框线。
    pub fn open_line(&self, horiz: bool, k: usize) -> Bitmap {
        let mut out = Bitmap::new(self.w, self.h);
        if k == 0 {
            return self.clone();
        }
        let (outer, inner) = if horiz {
            (self.h, self.w)
        } else {
            (self.w, self.h)
        };
        for o in 0..outer {
            let mut run = 0usize;
            for i in 0..=inner {
                let on = i < inner && if horiz { self.at(i, o) } else { self.at(o, i) } != 0;
                if on {
                    run += 1;
                } else {
                    if run >= k {
                        for j in (i - run)..i {
                            if horiz {
                                out.set(j, o, 1);
                            } else {
                                out.set(o, j, 1);
                            }
                        }
                    }
                    run = 0;
                }
            }
        }
        out
    }
}

/// 连通域的外接框与面积
#[derive(Debug, Clone, Copy)]
pub struct Blob {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
    pub area: usize,
}

/// 8 邻域连通域标记 —— 迭代式洪水填充, 不用递归(整页框线能有几万像素, 递归会爆栈)
pub fn connected_components(bm: &Bitmap) -> Vec<Blob> {
    let mut seen = vec![false; bm.w * bm.h];
    let mut out = Vec::new();
    let mut stack: Vec<(usize, usize)> = Vec::new();
    for y0 in 0..bm.h {
        for x0 in 0..bm.w {
            if bm.at(x0, y0) == 0 || seen[y0 * bm.w + x0] {
                continue;
            }
            seen[y0 * bm.w + x0] = true;
            stack.clear();
            stack.push((x0, y0));
            let (mut x1, mut y1, mut x2, mut y2, mut area) = (x0, y0, x0, y0, 0usize);
            while let Some((x, y)) = stack.pop() {
                area += 1;
                x1 = x1.min(x);
                y1 = y1.min(y);
                x2 = x2.max(x);
                y2 = y2.max(y);
                for dy in -1i32..=1 {
                    for dx in -1i32..=1 {
                        let nx = x as i32 + dx;
                        let ny = y as i32 + dy;
                        if nx < 0 || ny < 0 || nx >= bm.w as i32 || ny >= bm.h as i32 {
                            continue;
                        }
                        let (nx, ny) = (nx as usize, ny as usize);
                        let idx = ny * bm.w + nx;
                        if bm.px[idx] != 0 && !seen[idx] {
                            seen[idx] = true;
                            stack.push((nx, ny));
                        }
                    }
                }
            }
            out.push(Blob {
                x: x1,
                y: y1,
                w: x2 - x1 + 1,
                h: y2 - y1 + 1,
                area,
            });
        }
    }
    out
}

/// 连通域标记, 同时把每个域的像素坐标留下来 —— DB 检测的后处理要拿它算凸包
pub fn components_with_points(bm: &Bitmap) -> Vec<(Blob, Vec<(i32, i32)>)> {
    let mut seen = vec![false; bm.w * bm.h];
    let mut out = Vec::new();
    let mut stack: Vec<(usize, usize)> = Vec::new();
    for y0 in 0..bm.h {
        for x0 in 0..bm.w {
            if bm.at(x0, y0) == 0 || seen[y0 * bm.w + x0] {
                continue;
            }
            seen[y0 * bm.w + x0] = true;
            stack.clear();
            stack.push((x0, y0));
            let mut pts = Vec::new();
            let (mut x1, mut y1, mut x2, mut y2) = (x0, y0, x0, y0);
            while let Some((x, y)) = stack.pop() {
                pts.push((x as i32, y as i32));
                x1 = x1.min(x);
                y1 = y1.min(y);
                x2 = x2.max(x);
                y2 = y2.max(y);
                for dy in -1i32..=1 {
                    for dx in -1i32..=1 {
                        let nx = x as i32 + dx;
                        let ny = y as i32 + dy;
                        if nx < 0 || ny < 0 || nx >= bm.w as i32 || ny >= bm.h as i32 {
                            continue;
                        }
                        let (nx, ny) = (nx as usize, ny as usize);
                        let idx = ny * bm.w + nx;
                        if bm.px[idx] != 0 && !seen[idx] {
                            seen[idx] = true;
                            stack.push((nx, ny));
                        }
                    }
                }
            }
            let area = pts.len();
            out.push((
                Blob {
                    x: x1,
                    y: y1,
                    w: x2 - x1 + 1,
                    h: y2 - y1 + 1,
                    area,
                },
                pts,
            ));
        }
    }
    out
}

/// 3x3 膨胀 —— DB 后处理的 use_dilation
pub fn dilate3(bm: &Bitmap) -> Bitmap {
    let mut out = Bitmap::new(bm.w, bm.h);
    for y in 0..bm.h {
        for x in 0..bm.w {
            if bm.at(x, y) == 0 {
                continue;
            }
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let nx = x as i32 + dx;
                    let ny = y as i32 + dy;
                    if nx >= 0 && ny >= 0 && nx < bm.w as i32 && ny < bm.h as i32 {
                        out.set(nx as usize, ny as usize, 1);
                    }
                }
            }
        }
    }
    out
}
