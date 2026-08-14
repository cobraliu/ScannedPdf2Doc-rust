//! 检测框要用的几何: 凸包 / 最小外接矩形 / 四边形外扩 / 透视裁剪
//!
//! Python 版这几件事分别由 cv2.minAreaRect、pyclipper、cv2.warpPerspective 干。
//! 我们只对"四边形"做这些, 不需要通用多边形库, 手写百来行就够。

pub type Pt = (f32, f32);

/// 凸包(Andrew monotone chain), 返回逆时针点序
pub fn convex_hull(pts: &[(i32, i32)]) -> Vec<Pt> {
    if pts.len() < 3 {
        return pts.iter().map(|&(x, y)| (x as f32, y as f32)).collect();
    }
    let mut p: Vec<(i32, i32)> = pts.to_vec();
    p.sort_unstable();
    p.dedup();
    if p.len() < 3 {
        return p.iter().map(|&(x, y)| (x as f32, y as f32)).collect();
    }
    let cross = |o: (i32, i32), a: (i32, i32), b: (i32, i32)| -> i64 {
        (a.0 - o.0) as i64 * (b.1 - o.1) as i64 - (a.1 - o.1) as i64 * (b.0 - o.0) as i64
    };
    let mut hull: Vec<(i32, i32)> = Vec::with_capacity(p.len() * 2);
    for &pt in p.iter() {
        while hull.len() >= 2 && cross(hull[hull.len() - 2], hull[hull.len() - 1], pt) <= 0 {
            hull.pop();
        }
        hull.push(pt);
    }
    let lower = hull.len() + 1;
    for &pt in p.iter().rev() {
        while hull.len() >= lower && cross(hull[hull.len() - 2], hull[hull.len() - 1], pt) <= 0 {
            hull.pop();
        }
        hull.push(pt);
    }
    hull.pop();
    hull.into_iter()
        .map(|(x, y)| (x as f32, y as f32))
        .collect()
}

/// 最小外接矩形(旋转卡壳): 枚举凸包每条边作为矩形的一条边, 取面积最小的
///
/// 返回四个角点。等价于 cv2.minAreaRect + boxPoints。
pub fn min_area_rect(hull: &[Pt]) -> [Pt; 4] {
    if hull.len() < 3 {
        let (x0, y0, x1, y1) = bounds(hull);
        return [(x0, y0), (x1, y0), (x1, y1), (x0, y1)];
    }
    let n = hull.len();
    let mut best_area = f32::INFINITY;
    let mut best = [(0.0, 0.0); 4];
    for i in 0..n {
        let a = hull[i];
        let b = hull[(i + 1) % n];
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len = (dx * dx + dy * dy).sqrt();
        if len < 1e-6 {
            continue;
        }
        let (ux, uy) = (dx / len, dy / len); // 边方向
        let (vx, vy) = (-uy, ux); // 法向
        let (mut u0, mut u1, mut v0, mut v1) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
        for &(px, py) in hull {
            let pu = px * ux + py * uy;
            let pv = px * vx + py * vy;
            u0 = u0.min(pu);
            u1 = u1.max(pu);
            v0 = v0.min(pv);
            v1 = v1.max(pv);
        }
        let area = (u1 - u0) * (v1 - v0);
        if area < best_area {
            best_area = area;
            let mk = |u: f32, v: f32| (u * ux + v * vx, u * uy + v * vy);
            best = [mk(u0, v0), mk(u1, v0), mk(u1, v1), mk(u0, v1)];
        }
    }
    best
}

/// 把四个角点排成 左上→右上→右下→左下
pub fn order_quad(q: [Pt; 4]) -> [Pt; 4] {
    let mut p = q;
    p.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    let (mut l, mut r) = ([p[0], p[1]], [p[2], p[3]]);
    if l[0].1 > l[1].1 {
        l.swap(0, 1);
    }
    if r[0].1 > r[1].1 {
        r.swap(0, 1);
    }
    [l[0], r[0], r[1], l[1]]
}

/// 四边形按 unclip_ratio 外扩
///
/// PaddleOCR 用 pyclipper 做多边形偏移; 对凸四边形来说就是把每条边沿法向
/// 外移 d, 再求相邻边的交点。d = 面积 * ratio / 周长, 跟 pyclipper 一致。
pub fn unclip_quad(q: [Pt; 4], ratio: f32) -> [Pt; 4] {
    let area = polygon_area(&q).abs();
    let peri: f32 = (0..4)
        .map(|i| {
            let a = q[i];
            let b = q[(i + 1) % 4];
            ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt()
        })
        .sum();
    if peri < 1e-6 {
        return q;
    }
    let d = area * ratio / peri;
    // 保证点序是逆时针, 否则法向会朝内
    let ccw = polygon_area(&q) > 0.0;
    let mut lines = [((0.0f32, 0.0f32), (0.0f32, 0.0f32)); 4]; // (点, 方向)
    for i in 0..4 {
        let a = q[i];
        let b = q[(i + 1) % 4];
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len = (dx * dx + dy * dy).sqrt().max(1e-6);
        let (ux, uy) = (dx / len, dy / len);
        // 逆时针时外法向是 (uy, -ux)
        let (nx, ny) = if ccw { (uy, -ux) } else { (-uy, ux) };
        lines[i] = ((a.0 + nx * d, a.1 + ny * d), (ux, uy));
    }
    let mut out = [(0.0f32, 0.0f32); 4];
    for i in 0..4 {
        let (p1, d1) = lines[(i + 3) % 4];
        let (p2, d2) = lines[i];
        out[i] = intersect(p1, d1, p2, d2).unwrap_or(q[i]);
    }
    out
}

fn intersect(p1: Pt, d1: Pt, p2: Pt, d2: Pt) -> Option<Pt> {
    let det = d1.0 * d2.1 - d1.1 * d2.0;
    if det.abs() < 1e-6 {
        return None;
    }
    let t = ((p2.0 - p1.0) * d2.1 - (p2.1 - p1.1) * d2.0) / det;
    Some((p1.0 + d1.0 * t, p1.1 + d1.1 * t))
}

pub fn polygon_area(q: &[Pt]) -> f32 {
    let n = q.len();
    let mut s = 0.0;
    for i in 0..n {
        let a = q[i];
        let b = q[(i + 1) % n];
        s += a.0 * b.1 - b.0 * a.1;
    }
    s / 2.0
}

pub fn bounds(pts: &[Pt]) -> (f32, f32, f32, f32) {
    let mut r = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for &(x, y) in pts {
        r.0 = r.0.min(x);
        r.1 = r.1.min(y);
        r.2 = r.2.max(x);
        r.3 = r.3.max(y);
    }
    r
}

/// 按四边形做透视裁剪, 拉正成 w x h 的灰度小图
///
/// 对应 PaddleOCR 的 get_rotate_crop_image。扫描件普遍带零点几度歪斜, 直接按
/// 外接框裁会把相邻行的笔画一起裁进来, 识别率明显下降。
pub fn crop_quad(src: &crate::imgutil::Gray, q: [Pt; 4]) -> (usize, usize, Vec<u8>) {
    let dist = |a: Pt, b: Pt| ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
    let w = dist(q[0], q[1]).max(dist(q[3], q[2])).round().max(1.0) as usize;
    let h = dist(q[0], q[3]).max(dist(q[1], q[2])).round().max(1.0) as usize;
    let mut out = vec![255u8; w * h];
    // 双线性反向映射: 目标 (u,v) -> 源, 用双线性插值取值
    for j in 0..h {
        let ty = j as f32 / h.max(1) as f32;
        for i in 0..w {
            let tx = i as f32 / w.max(1) as f32;
            // 四边形上的双线性插值(上下两边按 tx 插, 再按 ty 插)
            let top = (
                q[0].0 + (q[1].0 - q[0].0) * tx,
                q[0].1 + (q[1].1 - q[0].1) * tx,
            );
            let bot = (
                q[3].0 + (q[2].0 - q[3].0) * tx,
                q[3].1 + (q[2].1 - q[3].1) * tx,
            );
            let sx = top.0 + (bot.0 - top.0) * ty;
            let sy = top.1 + (bot.1 - top.1) * ty;
            out[j * w + i] = sample(src, sx, sy);
        }
    }
    (w, h, out)
}

fn sample(g: &crate::imgutil::Gray, x: f32, y: f32) -> u8 {
    if x < 0.0 || y < 0.0 || x >= g.w as f32 - 1.0 || y >= g.h as f32 - 1.0 {
        let xi = (x.round().max(0.0) as usize).min(g.w.saturating_sub(1));
        let yi = (y.round().max(0.0) as usize).min(g.h.saturating_sub(1));
        return g.at(xi, yi);
    }
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let p = |dx: usize, dy: usize| g.at(x0 + dx, y0 + dy) as f32;
    let v = p(0, 0) * (1.0 - fx) * (1.0 - fy)
        + p(1, 0) * fx * (1.0 - fy)
        + p(0, 1) * (1.0 - fx) * fy
        + p(1, 1) * fx * fy;
    v.round().clamp(0.0, 255.0) as u8
}
